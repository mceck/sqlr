//! The table rebuild for changes SQLite's `ALTER TABLE` cannot make (changing a type,
//! nullability, default, key or constraint; dropping a column something refers to),
//! following the documented procedure (<https://sqlite.org/lang_altertable.html>, "Making
//! Other Kinds Of Table Schema Changes"):
//!
//! 1. turn foreign key enforcement off: with it on, dropping the old table would run the
//!    `ON DELETE` actions of the tables referencing it. SQLite ignores this pragma inside a
//!    transaction, so it runs alone ([`Placement::Alone`]) before the transaction, and the
//!    statement that turns enforcement back on runs as cleanup ([`Placement::Cleanup`]); a
//!    guard fails the transaction if enforcement is still on;
//! 2. drop the views and other tables' triggers that name the table
//!    ([`TableDef::dependents`]);
//! 3. create the new table under a temporary name and copy the rows, column by column
//!    (renamed columns from their old name); an `AUTOINCREMENT` table also takes the old
//!    table's `sqlite_sequence` high-water mark, so ids of deleted rows are never reused;
//! 4. drop the old table, which drops its indexes and triggers, and give the new table the
//!    old name;
//! 5. create the indexes and triggers of the new definition and the dependents again; a
//!    guard reads every dependent view, so a view the change breaks fails the transaction.
//!    SQLite resolves a trigger's body only when it fires, so when triggers were created
//!    again a scratch table is created, renamed and dropped: `ALTER TABLE … RENAME`
//!    resolves every trigger and view of the schema and fails on a column or table that no
//!    longer exists;
//! 6. a guard runs `foreign_key_check` on the table and the tables referencing it;
//! 7. rename the table when its name changes: SQLite carries the rename into views,
//!    triggers and foreign keys.
//!
//! [`Placement::Alone`]: sqlr_core::Placement::Alone
//! [`Placement::Cleanup`]: sqlr_core::Placement::Cleanup

use sqlr_core::{
    Dialect, DriverError, Identity, ObjectKind, TableDef, TableRef, TriggerDef, pair_columns,
};

use crate::dialect::SqliteDialect;

/// Runs alone, before the transaction.
pub(crate) const FOREIGN_KEYS_OFF: &str = "PRAGMA foreign_keys = OFF";
/// Runs as cleanup, after the transaction whatever its outcome.
pub(crate) const FOREIGN_KEYS_ON: &str = "PRAGMA foreign_keys = ON";

/// Statements that fail with `CHECK constraint failed: <message>` unless `count` (a
/// `SELECT` of one number) yields 0.
fn guard(d: &SqliteDialect, message: &str, count: &str) -> Vec<String> {
    vec![
        format!(
            "CREATE TEMP TABLE \"sqlr_guard\" (\"n\" INTEGER CONSTRAINT {} CHECK (\"n\" = 0))",
            d.quote_ident(message)
        ),
        format!("INSERT INTO temp.\"sqlr_guard\" {count}"),
        "DROP TABLE temp.\"sqlr_guard\"".into(),
    ]
}

pub(crate) fn rebuild(
    d: &SqliteDialect,
    before: &TableDef,
    after: &TableDef,
) -> Result<Vec<String>, DriverError> {
    let schema = before.table.schema.clone().unwrap_or_else(|| "main".into());
    let in_schema = |name: &str| TableRef {
        database: None,
        schema: Some(schema.clone()),
        name: name.to_string(),
    };
    // The table is rebuilt under its current name; a new name comes last.
    let current = in_schema(&before.table.name);
    let temporary = in_schema(&format!("sqlr_new_{}", before.table.name));

    let mut out = vec![FOREIGN_KEYS_OFF.to_string()];
    out.extend(guard(
        d,
        "foreign keys must be off to rebuild a table",
        "SELECT foreign_keys FROM pragma_foreign_keys",
    ));
    for dependent in before.dependents.iter().rev() {
        out.push(format!(
            "DROP {} {}",
            dependent.kind.keyword(),
            d.qualified(&in_schema(&dependent.name))
        ));
    }

    let new_table = TableDef {
        table: temporary.clone(),
        indexes: Vec::new(),
        triggers: Vec::new(),
        ..after.clone()
    };
    out.push(d.create_table_statement(&new_table)?);
    let (targets, sources): (Vec<String>, Vec<String>) = pair_columns(before, after)
        .into_iter()
        .filter_map(|pair| match pair {
            (Some(b), Some(a)) if b.generated.is_none() && a.generated.is_none() => {
                Some((d.quote_ident(&a.name), d.quote_ident(&b.name)))
            }
            _ => None,
        })
        .unzip();
    if !targets.is_empty() {
        out.push(format!(
            "INSERT INTO {} ({}) SELECT {} FROM {}",
            d.qualified(&temporary),
            targets.join(", "),
            sources.join(", "),
            d.qualified(&current)
        ));
    }
    if after
        .columns
        .iter()
        .any(|c| c.identity == Some(Identity::AutoIncrement))
    {
        // Creating the new table created `sqlite_sequence`; dropping the old one deletes
        // its row.
        let sequence = format!("{}.sqlite_sequence", d.quote_ident(&schema));
        let new_name = d.string_literal(&temporary.name);
        let old_name = d.string_literal(&current.name);
        out.push(format!(
            "INSERT INTO {sequence} (name, seq) SELECT {new_name}, 0 \
             WHERE NOT EXISTS (SELECT 1 FROM {sequence} WHERE name = {new_name})"
        ));
        out.push(format!(
            "UPDATE {sequence} SET seq = max(seq, coalesce(\
             (SELECT max(seq) FROM {sequence} WHERE name = {old_name}), 0)) \
             WHERE name = {new_name}"
        ));
    }
    out.push(format!("DROP TABLE {}", d.qualified(&current)));
    out.push(format!(
        "ALTER TABLE {} RENAME TO {}",
        d.qualified(&temporary),
        d.quote_ident(&current.name)
    ));

    for index in &after.indexes {
        out.push(d.create_index(&current, index)?);
    }
    for trigger in &after.triggers {
        out.push(d.create_trigger(&TriggerDef {
            table: current.clone(),
            ..trigger.clone()
        })?);
    }
    for dependent in &before.dependents {
        out.push(dependent.sql.clone());
    }
    let views: Vec<String> = before
        .dependents
        .iter()
        .filter(|dep| dep.kind == ObjectKind::View)
        .map(|dep| {
            format!(
                "(SELECT count(*) FROM (SELECT * FROM {} LIMIT 0))",
                d.qualified(&in_schema(&dep.name))
            )
        })
        .collect();
    if !views.is_empty() {
        out.extend(guard(
            d,
            "a view naming the table no longer works",
            &format!("SELECT {}", views.join(" + ")),
        ));
    }
    let recreated_triggers = !after.triggers.is_empty()
        || before
            .dependents
            .iter()
            .any(|dep| dep.kind == ObjectKind::Trigger);
    if recreated_triggers {
        let check = in_schema(&format!("sqlr_check_{}", before.table.name));
        let checked = format!("sqlr_checked_{}", before.table.name);
        out.push(format!("CREATE TABLE {} (\"n\")", d.qualified(&check)));
        out.push(format!(
            "ALTER TABLE {} RENAME TO {}",
            d.qualified(&check),
            d.quote_ident(&checked)
        ));
        out.push(format!("DROP TABLE {}", d.qualified(&in_schema(&checked))));
    }

    let name = d.string_literal(&current.name);
    let schema_literal = d.string_literal(&schema);
    let schema_table = format!("{}.sqlite_schema", d.quote_ident(&schema));
    out.extend(guard(
        d,
        "foreign keys would be violated: see PRAGMA foreign_key_check",
        &format!(
            "SELECT (SELECT count(*) FROM pragma_foreign_key_check({name}, {schema_literal})) + \
             (SELECT count(*) FROM {schema_table} m, \
              pragma_foreign_key_check(m.name, {schema_literal}) c \
              WHERE m.type = 'table' AND c.\"parent\" = {name} COLLATE NOCASE \
                AND m.name IN (SELECT m2.name FROM {schema_table} m2, \
                               pragma_foreign_key_list(m2.name, {schema_literal}) l \
                               WHERE m2.type = 'table' \
                                 AND l.\"table\" = {name} COLLATE NOCASE))"
        ),
    ));

    if after.table.name != before.table.name {
        out.push(format!(
            "ALTER TABLE {} RENAME TO {}",
            d.qualified(&current),
            d.quote_ident(&after.table.name)
        ));
    }
    out.push(FOREIGN_KEYS_ON.to_string());
    Ok(out)
}
