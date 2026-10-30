//! DDL generation for SQLite. Identifiers go through [`Dialect::quote_ident`]; expression
//! fields are written as is. Table changes `ALTER TABLE` cannot express go through the
//! table rebuild ([`crate::rebuild`]), reached with [`DdlGen::table_ddl`].

use sqlr_core::{
    ColumnDef, ConstraintDef, DdlGen, Dialect, DriverError, ErrorKind, FkAction, Identity,
    IndexDef, ObjectDef, ObjectKind, ObjectRef, Placement, SchemaChange, TableDef, TableRef,
    TriggerDef, ViewDef, diff_table, script,
};

use crate::dialect::SqliteDialect;
use crate::rebuild;

fn unsupported(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Unsupported, message)
}

fn needs_rebuild(what: &str) -> DriverError {
    unsupported(format!(
        "SQLite cannot {what} with ALTER TABLE: edit the table to rebuild it"
    ))
}

impl SqliteDialect {
    fn q(&self, ident: &str) -> String {
        self.quote_ident(ident)
    }

    fn quoted_list(&self, names: &[String]) -> String {
        names
            .iter()
            .map(|n| self.q(n))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `"schema"."name"`; SQLite schemas are `main`, `temp` or attached databases.
    fn name_of(&self, table: &TableRef) -> String {
        self.qualified(table)
    }

    /// A column; `primary` is the `PRIMARY KEY AUTOINCREMENT` clause it carries.
    fn column_definition(&self, c: &ColumnDef, primary: Option<&str>) -> String {
        let mut out = self.q(&c.name);
        if !c.type_name.is_empty() {
            out.push(' ');
            out.push_str(&c.type_name);
        }
        if let Some(primary) = primary {
            out.push(' ');
            out.push_str(primary);
        }
        if !c.nullable {
            out.push_str(" NOT NULL");
        }
        if let Some(default) = &c.default {
            out.push_str(&format!(" DEFAULT {default}"));
        }
        if let Some(collation) = &c.collation {
            out.push_str(&format!(" COLLATE {}", self.q(collation)));
        }
        if let Some(expr) = &c.generated {
            let storage = if c.generated_stored {
                "STORED"
            } else {
                "VIRTUAL"
            };
            out.push_str(&format!(" GENERATED ALWAYS AS ({expr}) {storage}"));
        }
        out
    }

    fn constraint(&self, c: &ConstraintDef) -> String {
        let named = match c.name() {
            Some(n) => format!("CONSTRAINT {} ", self.q(n)),
            None => String::new(),
        };
        let body = match c {
            ConstraintDef::PrimaryKey { columns, .. } => {
                format!("PRIMARY KEY ({})", self.quoted_list(columns))
            }
            ConstraintDef::Unique { columns, .. } => {
                format!("UNIQUE ({})", self.quoted_list(columns))
            }
            ConstraintDef::Check { expression, .. } => format!("CHECK ({expression})"),
            ConstraintDef::ForeignKey {
                columns,
                target,
                target_columns,
                on_update,
                on_delete,
                deferrable,
                initially_deferred,
                ..
            } => {
                // The parent is in the same schema and is named without it.
                let mut fk = format!(
                    "FOREIGN KEY ({}) REFERENCES {}",
                    self.quoted_list(columns),
                    self.q(&target.name)
                );
                if !target_columns.is_empty() {
                    fk.push_str(&format!(" ({})", self.quoted_list(target_columns)));
                }
                if *on_delete != FkAction::NoAction {
                    fk.push_str(&format!(" ON DELETE {}", on_delete.sql()));
                }
                if *on_update != FkAction::NoAction {
                    fk.push_str(&format!(" ON UPDATE {}", on_update.sql()));
                }
                if *deferrable {
                    fk.push_str(" DEFERRABLE");
                    if *initially_deferred {
                        fk.push_str(" INITIALLY DEFERRED");
                    }
                }
                fk
            }
            ConstraintDef::Raw { definition, .. } => definition.clone(),
        };
        format!("{named}{body}")
    }

    /// `CREATE TABLE` alone (no indexes or triggers).
    pub(crate) fn create_table_statement(&self, t: &TableDef) -> Result<String, DriverError> {
        // An AUTOINCREMENT column carries the primary key itself.
        let auto = t
            .columns
            .iter()
            .find(|c| c.identity == Some(Identity::AutoIncrement));
        let pk = t.constraints.iter().find_map(|c| match c {
            ConstraintDef::PrimaryKey { name, columns } => Some((name, columns)),
            _ => None,
        });
        let auto_clause = match (auto, pk) {
            (Some(c), Some((name, columns)))
                if c.type_name.trim().eq_ignore_ascii_case("INTEGER")
                    && columns.as_slice() == [c.name.clone()] =>
            {
                let named = match name {
                    Some(n) => format!("CONSTRAINT {} ", self.q(n)),
                    None => String::new(),
                };
                Some((c.name.clone(), format!("{named}PRIMARY KEY AUTOINCREMENT")))
            }
            (Some(_), _) => {
                return Err(unsupported(
                    "AUTOINCREMENT needs a one-column INTEGER primary key",
                ));
            }
            (None, _) => None,
        };
        let mut lines = Vec::new();
        for c in &t.columns {
            if matches!(c.identity, Some(Identity::Always | Identity::ByDefault)) {
                return Err(unsupported(
                    "SQLite has no identity columns: use INTEGER PRIMARY KEY AUTOINCREMENT",
                ));
            }
            let primary = auto_clause
                .as_ref()
                .filter(|(name, _)| *name == c.name)
                .map(|(_, clause)| clause.as_str());
            lines.push(format!("    {}", self.column_definition(c, primary)));
        }
        for c in &t.constraints {
            if auto_clause.is_some() && matches!(c, ConstraintDef::PrimaryKey { .. }) {
                continue;
            }
            lines.push(format!("    {}", self.constraint(c)));
        }
        let mut create = format!(
            "CREATE TABLE {} (\n{}\n)",
            self.name_of(&t.table),
            lines.join(",\n")
        );
        let mut options = Vec::new();
        for (key, value) in &t.options {
            match (key.as_str(), value.as_str()) {
                ("without_rowid", "true") => options.push("WITHOUT ROWID"),
                ("strict", "true") => options.push("STRICT"),
                (_, "false") => {}
                _ => return Err(unsupported(format!("unknown table option `{key}`"))),
            }
        }
        if !options.is_empty() {
            create.push(' ');
            create.push_str(&options.join(", "));
        }
        if t.columns.iter().any(|c| c.on_update.is_some()) {
            return Err(unsupported("SQLite has no ON UPDATE: use a trigger"));
        }
        if t.comment.is_some() || t.columns.iter().any(|c| c.comment.is_some()) {
            return Err(unsupported("SQLite has no comments"));
        }
        if !t.partitions.is_empty() {
            return Err(unsupported("SQLite has no partitions"));
        }
        Ok(create)
    }

    pub(crate) fn create_index(
        &self,
        table: &TableRef,
        i: &IndexDef,
    ) -> Result<String, DriverError> {
        if let Some(definition) = &i.definition {
            return Ok(definition.clone());
        }
        if i.method.is_some() || !i.include.is_empty() {
            return Err(unsupported(
                "SQLite indexes have no access method or INCLUDE columns",
            ));
        }
        let unique = if i.unique { "UNIQUE " } else { "" };
        let parts: Vec<String> = i
            .columns
            .iter()
            .map(|c| {
                let expr = if c.is_expression() {
                    c.expr.clone()
                } else {
                    self.q(&c.expr)
                };
                if c.desc { format!("{expr} DESC") } else { expr }
            })
            .collect();
        let mut out = format!(
            "CREATE {unique}INDEX {} ON {} ({})",
            self.name_of(&TableRef {
                name: i.name.clone(),
                ..table.clone()
            }),
            self.q(&table.name),
            parts.join(", ")
        );
        if let Some(predicate) = &i.predicate {
            out.push_str(&format!(" WHERE {predicate}"));
        }
        Ok(out)
    }

    pub(crate) fn create_trigger(&self, t: &TriggerDef) -> Result<String, DriverError> {
        let [event] = t.events.as_slice() else {
            return Err(unsupported("a SQLite trigger fires on exactly one event"));
        };
        if !t.for_each_row {
            return Err(unsupported("SQLite triggers are FOR EACH ROW"));
        }
        let mut out = format!(
            "CREATE TRIGGER {} {} {event} ON {} FOR EACH ROW",
            self.name_of(&TableRef {
                name: t.name.clone(),
                ..t.table.clone()
            }),
            t.timing,
            self.q(&t.table.name)
        );
        if let Some(condition) = &t.condition {
            out.push_str(&format!(" WHEN {condition}"));
        }
        out.push_str(&format!("\nBEGIN\n{}\nEND", t.body));
        Ok(out)
    }

    fn create_view(&self, v: &ViewDef) -> Result<String, DriverError> {
        if v.materialized {
            return Err(unsupported("SQLite has no materialized views"));
        }
        if v.comment.is_some() {
            return Err(unsupported("SQLite has no comments"));
        }
        let mut out = format!("CREATE VIEW {}", self.name_of(&v.view));
        if !v.columns.is_empty() {
            out.push_str(&format!(" ({})", self.quoted_list(&v.columns)));
        }
        out.push_str(&format!(" AS {}", v.query));
        Ok(out)
    }

    fn create(&self, def: &ObjectDef) -> Result<Vec<String>, DriverError> {
        Ok(match def {
            ObjectDef::Table(t) => {
                let mut out = vec![self.create_table_statement(t)?];
                for i in &t.indexes {
                    out.push(self.create_index(&t.table, i)?);
                }
                for trigger in &t.triggers {
                    out.push(self.create_trigger(trigger)?);
                }
                out
            }
            ObjectDef::View(v) => vec![self.create_view(v)?],
            ObjectDef::Index { table, index } => vec![self.create_index(table, index)?],
            ObjectDef::Trigger(t) => vec![self.create_trigger(t)?],
            other => {
                return Err(unsupported(format!(
                    "SQLite cannot create a {}",
                    other.kind().keyword().to_lowercase()
                )));
            }
        })
    }

    /// `"schema"."name"` of a table, view, index or trigger.
    fn object_name(&self, o: &ObjectRef) -> String {
        self.name_of(&o.table_ref())
    }

    fn change(&self, change: &SchemaChange) -> Result<Vec<String>, DriverError> {
        Ok(match change {
            SchemaChange::Create(def) => self.create(def)?,
            SchemaChange::Drop { object, .. } => match object.kind {
                ObjectKind::Table | ObjectKind::View | ObjectKind::Index | ObjectKind::Trigger => {
                    vec![format!(
                        "DROP {} {}",
                        object.kind.keyword(),
                        self.object_name(object)
                    )]
                }
                other => {
                    return Err(unsupported(format!(
                        "SQLite cannot drop a {}",
                        other.keyword().to_lowercase()
                    )));
                }
            },
            SchemaChange::Rename { object, to } if object.kind == ObjectKind::Table => {
                vec![format!(
                    "ALTER TABLE {} RENAME TO {}",
                    self.object_name(object),
                    self.q(to)
                )]
            }
            SchemaChange::Rename { object, .. } => {
                return Err(unsupported(format!(
                    "SQLite cannot rename a {}: drop and create it",
                    object.kind.keyword().to_lowercase()
                )));
            }
            SchemaChange::AddColumn { table, column } => {
                if !addable(column) {
                    return Err(needs_rebuild("add this column"));
                }
                vec![format!(
                    "ALTER TABLE {} ADD COLUMN {}",
                    self.name_of(table),
                    self.column_definition(column, None)
                )]
            }
            SchemaChange::DropColumn { table, name } => vec![format!(
                "ALTER TABLE {} DROP COLUMN {}",
                self.name_of(table),
                self.q(name)
            )],
            SchemaChange::AlterColumn {
                table,
                before,
                after,
            } => {
                if !rename_only(before, after) {
                    return Err(needs_rebuild("change a column"));
                }
                vec![format!(
                    "ALTER TABLE {} RENAME COLUMN {} TO {}",
                    self.name_of(table),
                    self.q(&before.name),
                    self.q(&after.name)
                )]
            }
            SchemaChange::AddIndex { table, index, .. } => vec![self.create_index(table, index)?],
            SchemaChange::DropIndex { table, name } => vec![format!(
                "DROP INDEX {}",
                self.name_of(&TableRef {
                    name: name.clone(),
                    ..table.clone()
                })
            )],
            SchemaChange::ReplaceView(v) => vec![
                format!("DROP VIEW {}", self.name_of(&v.view)),
                self.create_view(v)?,
            ],
            SchemaChange::AddConstraint { .. } => return Err(needs_rebuild("add a constraint")),
            SchemaChange::DropConstraint { .. } => return Err(needs_rebuild("drop a constraint")),
            SchemaChange::SetOptions { .. } => return Err(needs_rebuild("change table options")),
            SchemaChange::SetComment { .. } => return Err(unsupported("SQLite has no comments")),
            SchemaChange::ReplaceRoutine(_) => {
                return Err(unsupported("SQLite has no functions or procedures"));
            }
            SchemaChange::AttachPartition { .. } | SchemaChange::DetachPartition { .. } => {
                return Err(unsupported("SQLite has no partitions"));
            }
            // SQLite has no TRUNCATE (nor CASCADE): its truncate optimization deletes.
            SchemaChange::Truncate { table, .. } => {
                vec![format!("DELETE FROM {}", self.name_of(table))]
            }
        })
    }
}

/// `ALTER TABLE … ADD COLUMN` accepts the column (SQLite's documented restrictions).
fn addable(c: &ColumnDef) -> bool {
    let constant_default = c.default.as_deref().is_none_or(|d| {
        let upper = d.trim().to_ascii_uppercase();
        !upper.starts_with('(') && !upper.starts_with("CURRENT_")
    });
    c.identity.is_none()
        && !(c.generated.is_some() && c.generated_stored)
        && (c.nullable || c.default.is_some() || c.generated.is_some())
        && constant_default
}

/// Only the name differs.
fn rename_only(before: &ColumnDef, after: &ColumnDef) -> bool {
    ColumnDef {
        name: after.name.clone(),
        ..before.clone()
    } == *after
}

/// `ALTER TABLE … DROP COLUMN` can drop `name` of `table`: nothing else refers to it.
fn droppable(table: &TableDef, name: &str) -> bool {
    let in_constraint = table
        .constraints
        .iter()
        .any(|c| matches!(c, ConstraintDef::Check { .. }) || c.columns().iter().any(|n| n == name));
    let in_index = table.indexes.iter().any(|i| {
        i.predicate.is_some()
            || i.columns
                .iter()
                .any(|c| c.is_expression() || c.expr == name)
    });
    let generated = table.columns.iter().any(|c| c.generated.is_some());
    !in_constraint && !in_index && !generated && table.triggers.is_empty()
}

/// True when the constraints change, column renames aside. SQLite keeps constraints
/// unnamed, and `diff_table` has no change for dropping an unnamed one.
fn constraints_differ(before: &TableDef, after: &TableDef) -> bool {
    let renames: Vec<(&str, &str)> = sqlr_core::pair_columns(before, after)
        .into_iter()
        .filter_map(|pair| match pair {
            (Some(b), Some(a)) if b.name != a.name => Some((b.name.as_str(), a.name.as_str())),
            _ => None,
        })
        .collect();
    let rename = |columns: &mut Vec<String>| {
        for c in columns.iter_mut() {
            if let Some((_, to)) = renames.iter().find(|(from, _)| *from == c.as_str()) {
                *c = to.to_string();
            }
        }
    };
    let renamed: Vec<ConstraintDef> = before
        .constraints
        .iter()
        .cloned()
        .map(|mut c| {
            match &mut c {
                ConstraintDef::PrimaryKey { columns, .. }
                | ConstraintDef::Unique { columns, .. }
                | ConstraintDef::ForeignKey { columns, .. } => rename(columns),
                ConstraintDef::Check { .. } | ConstraintDef::Raw { .. } => {}
            }
            c
        })
        .collect();
    renamed.len() != after.constraints.len()
        || renamed.iter().any(|c| !after.constraints.contains(c))
}

/// True when `change` (part of `before` → after) runs as a plain `ALTER TABLE`.
fn native(change: &SchemaChange, before: &TableDef) -> bool {
    match change {
        SchemaChange::Rename { object, .. } => {
            matches!(object.kind, ObjectKind::Table | ObjectKind::Index)
        }
        SchemaChange::AddColumn { column, .. } => addable(column),
        SchemaChange::DropColumn { name, .. } => droppable(before, name),
        SchemaChange::AlterColumn { before, after, .. } => rename_only(before, after),
        SchemaChange::AddIndex { .. } | SchemaChange::DropIndex { .. } => true,
        SchemaChange::Drop { object, .. } => object.kind == ObjectKind::Trigger,
        SchemaChange::Create(ObjectDef::Trigger(_)) => true,
        _ => false,
    }
}

impl DdlGen for SqliteDialect {
    fn ddl(&self, changes: &[SchemaChange]) -> Result<Vec<String>, DriverError> {
        let mut out = Vec::new();
        for change in changes {
            out.extend(self.change(change)?);
        }
        Ok(out)
    }

    fn create_ddl(&self, def: &ObjectDef) -> Result<String, DriverError> {
        Ok(script(&self.create(def)?))
    }

    /// SQLite ignores `PRAGMA foreign_keys` inside a transaction: the rebuild turns
    /// enforcement off alone, before its transaction, and back on as cleanup.
    fn placement(&self, statement: &str) -> Placement {
        match statement {
            rebuild::FOREIGN_KEYS_OFF => Placement::Alone,
            rebuild::FOREIGN_KEYS_ON => Placement::Cleanup,
            _ => Placement::Transaction,
        }
    }

    /// Plain `ALTER TABLE` statements when SQLite can express every change, the table
    /// rebuild otherwise.
    fn table_ddl(&self, before: &TableDef, after: &TableDef) -> Result<Vec<String>, DriverError> {
        let changes = diff_table(before, after);
        if !changes.iter().all(|c| native(c, before))
            || constraints_differ(before, after)
            || sqlr_core::column_order_changed(before, after)
        {
            return rebuild::rebuild(self, before, after);
        }
        let mut out = Vec::new();
        let mut recreated = Vec::new();
        for change in &changes {
            match change {
                // SQLite cannot rename an index: it is dropped and created again, once the
                // columns it names have their new names.
                SchemaChange::Rename { object, to } if object.kind == ObjectKind::Index => {
                    let index = after
                        .indexes
                        .iter()
                        .find(|i| i.name == *to)
                        .ok_or_else(|| unsupported(format!("index {to} not found")))?;
                    out.push(format!(
                        "DROP INDEX {}",
                        self.name_of(&TableRef {
                            name: object.name.clone(),
                            ..after.table.clone()
                        })
                    ));
                    recreated.push(self.create_index(&after.table, index)?);
                }
                // The editor may leave a trigger's table at the name the table had when
                // the trigger was added: it belongs to this table, whatever it says.
                SchemaChange::Create(ObjectDef::Trigger(trigger)) => {
                    out.push(self.create_trigger(&TriggerDef {
                        table: after.table.clone(),
                        ..trigger.clone()
                    })?);
                }
                other => out.extend(self.change(other)?),
            }
        }
        out.extend(recreated);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::IndexColumn;

    fn t(name: &str) -> TableRef {
        TableRef {
            database: None,
            schema: Some("main".into()),
            name: name.into(),
        }
    }

    #[test]
    fn autoincrement_carries_the_primary_key() {
        let mut def = TableDef::new(t("a\"b"));
        def.columns = vec![ColumnDef {
            name: "id".into(),
            type_name: "INTEGER".into(),
            nullable: true,
            identity: Some(Identity::AutoIncrement),
            ..Default::default()
        }];
        def.constraints.push(ConstraintDef::PrimaryKey {
            name: None,
            columns: vec!["id".into()],
        });
        def.indexes.push(IndexDef {
            name: "i".into(),
            columns: vec![IndexColumn::column("id")],
            ..Default::default()
        });
        assert_eq!(
            SqliteDialect
                .create_ddl(&ObjectDef::Table(def.clone()))
                .unwrap(),
            "CREATE TABLE \"main\".\"a\"\"b\" (\n    \"id\" INTEGER PRIMARY KEY AUTOINCREMENT\n);\n\n\
             CREATE INDEX \"main\".\"i\" ON \"a\"\"b\" (\"id\");"
        );
        def.comment = Some("x".into());
        assert_eq!(
            SqliteDialect
                .create_ddl(&ObjectDef::Table(def))
                .unwrap_err()
                .kind,
            ErrorKind::Unsupported
        );
    }

    #[test]
    fn autoincrement_needs_an_integer_column() {
        let mut def = TableDef::new(t("t"));
        def.columns = vec![ColumnDef {
            name: "id".into(),
            type_name: "BIGINT".into(),
            nullable: true,
            identity: Some(Identity::AutoIncrement),
            ..Default::default()
        }];
        def.constraints.push(ConstraintDef::PrimaryKey {
            name: None,
            columns: vec!["id".into()],
        });
        // SQLite accepts AUTOINCREMENT only on an exact INTEGER primary key; emitting
        // `BIGINT PRIMARY KEY AUTOINCREMENT` would fail on apply.
        let err = SqliteDialect
            .create_table_statement(&def)
            .expect_err("a non-INTEGER AUTOINCREMENT column cannot be created");
        assert!(err.message.contains("INTEGER"), "{}", err.message);
    }

    #[test]
    fn rebuild_turns_foreign_keys_off_alone_and_on_as_cleanup() {
        let mut before = TableDef::new(t("x"));
        before.columns = vec![ColumnDef {
            name: "a".into(),
            type_name: "TEXT".into(),
            nullable: true,
            ..Default::default()
        }];
        let mut after = before.clone();
        after.columns[0].type_name = "INTEGER".into();
        let statements = SqliteDialect.table_ddl(&before, &after).unwrap();
        let placements: Vec<Placement> = statements
            .iter()
            .map(|s| SqliteDialect.placement(s))
            .collect();
        assert_eq!(
            placements.first(),
            Some(&Placement::Alone),
            "{statements:#?}"
        );
        assert_eq!(
            placements.last(),
            Some(&Placement::Cleanup),
            "{statements:#?}"
        );
        assert!(
            placements[1..placements.len() - 1]
                .iter()
                .all(|p| *p == Placement::Transaction),
            "{statements:#?}"
        );
    }

    #[test]
    fn changes_alter_table_cannot_express_need_a_rebuild() {
        let column = ColumnDef {
            name: "c".into(),
            type_name: "TEXT".into(),
            nullable: false,
            ..Default::default()
        };
        let err = SqliteDialect
            .ddl(&[SchemaChange::AddColumn {
                table: t("x"),
                column,
            }])
            .unwrap_err();
        assert!(err.message.contains("rebuild"), "{err:?}");
    }

    #[test]
    fn reordering_columns_rebuilds() {
        let col = |name: &str| ColumnDef {
            name: name.into(),
            type_name: "TEXT".into(),
            nullable: true,
            ..Default::default()
        };
        let mut before = TableDef::new(t("x"));
        before.columns = vec![col("a"), col("b")];
        let mut after = before.clone();
        after.columns.swap(0, 1);
        let statements = SqliteDialect.table_ddl(&before, &after).unwrap();
        assert!(
            statements
                .iter()
                .any(|s| s.starts_with("CREATE TABLE") && s.contains("\"b\" TEXT,\n    \"a\" TEXT")),
            "{statements:#?}"
        );
    }

    #[test]
    fn truncate_deletes_every_row() {
        let truncate = |cascade| SchemaChange::Truncate {
            table: t("x"),
            cascade,
        };
        let plain = SqliteDialect.ddl(&[truncate(false)]).unwrap();
        assert_eq!(plain, ["DELETE FROM \"main\".\"x\""]);
        assert_eq!(SqliteDialect.ddl(&[truncate(true)]).unwrap(), plain);
    }
}
