//! Object definitions from `pg_catalog`. All catalog queries are parameterized.

use sqlr_core::{
    ColumnDef, ConstraintDef, DatabaseDef, Dialect, DriverError, ErrorKind, FkAction, Identity,
    IndexColumn, IndexDef, ObjectDef, ObjectKind, ObjectRef, PartitionDef, RoutineDef, SchemaDef,
    SequenceDef, TableDef, TableRef, TriggerDef, ViewDef,
};
use tokio_postgres::Client;

use crate::describe::{PgColumn, columns};
use crate::dialect::PgDialect;
use crate::map_error;

fn not_found(what: &str, name: &str) -> DriverError {
    DriverError::new(ErrorKind::Other, format!("{what} {name} not found"))
}

fn table_ref(schema: &str, name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some(schema.to_string()),
        name: name.to_string(),
    }
}

pub(crate) async fn load_object(
    client: &Client,
    object: &ObjectRef,
) -> Result<ObjectDef, DriverError> {
    let schema = match &object.schema {
        Some(s) => s.clone(),
        None => client
            .query_one("SELECT current_schema()::text", &[])
            .await
            .map_err(map_error)?
            .get(0),
    };
    let name = object.name.as_str();
    match object.kind {
        ObjectKind::Database => database(client, name).await,
        ObjectKind::Schema => schema_def(client, name).await,
        ObjectKind::Table => Ok(ObjectDef::Table(table(client, &schema, name).await?)),
        ObjectKind::View | ObjectKind::MaterializedView => view(client, &schema, name).await,
        ObjectKind::Index => index(client, &schema, name).await,
        ObjectKind::Sequence => sequence(client, &schema, name).await,
        ObjectKind::Function | ObjectKind::Procedure => {
            routine(client, &schema, name, object.signature.as_deref()).await
        }
        ObjectKind::Trigger => {
            let parent = object.parent.as_deref().ok_or_else(|| {
                DriverError::new(ErrorKind::Other, "a trigger is named with its table")
            })?;
            let oid = relation_oid(client, &schema, parent, &["r", "p", "v", "f"])
                .await?
                .ok_or_else(|| not_found("table", parent))?;
            triggers(client, oid, &table_ref(&schema, parent))
                .await?
                .into_iter()
                .find(|t| t.name == name)
                .map(ObjectDef::Trigger)
                .ok_or_else(|| not_found("trigger", name))
        }
        ObjectKind::Role => Err(DriverError::new(
            ErrorKind::Unsupported,
            "roles are loaded through administration",
        )),
    }
}

pub(crate) async fn roles(client: &Client) -> Result<Vec<String>, DriverError> {
    let rows = client
        .query("SELECT rolname::text FROM pg_roles ORDER BY rolname", &[])
        .await
        .map_err(map_error)?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

pub(crate) async fn relation_oid(
    client: &Client,
    schema: &str,
    name: &str,
    relkinds: &[&str],
) -> Result<Option<u32>, DriverError> {
    let kinds: Vec<String> = relkinds.iter().map(|k| k.to_string()).collect();
    let row = client
        .query_opt(
            "SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind::text = ANY($3)",
            &[&schema, &name, &kinds],
        )
        .await
        .map_err(map_error)?;
    Ok(row.map(|r| r.get(0)))
}

async fn database(client: &Client, name: &str) -> Result<ObjectDef, DriverError> {
    let row = client
        .query_opt(
            "SELECT pg_get_userbyid(datdba)::text, pg_encoding_to_char(encoding)::text, \
                    shobj_description(oid, 'pg_database') \
             FROM pg_database WHERE datname = $1",
            &[&name],
        )
        .await
        .map_err(map_error)?
        .ok_or_else(|| not_found("database", name))?;
    let mut options = vec![
        ("owner".to_string(), row.get::<_, String>(0)),
        ("encoding".to_string(), row.get::<_, String>(1)),
    ];
    if let Some(comment) = row.get::<_, Option<String>>(2) {
        options.push(("comment".into(), comment));
    }
    Ok(ObjectDef::Database(DatabaseDef {
        name: name.to_string(),
        options,
    }))
}

async fn schema_def(client: &Client, name: &str) -> Result<ObjectDef, DriverError> {
    let row = client
        .query_opt(
            "SELECT pg_get_userbyid(nspowner)::text, obj_description(oid, 'pg_namespace') \
             FROM pg_namespace WHERE nspname = $1",
            &[&name],
        )
        .await
        .map_err(map_error)?
        .ok_or_else(|| not_found("schema", name))?;
    Ok(ObjectDef::Schema(SchemaDef {
        name: name.to_string(),
        owner: row.get(0),
        comment: row.get(1),
    }))
}

/// A table column as the model has it.
pub(crate) fn column_def(c: PgColumn) -> ColumnDef {
    let generated = !c.generated.is_empty();
    ColumnDef {
        name: c.name,
        type_name: c.type_name,
        nullable: !c.not_null,
        default: c.expression.clone().filter(|_| !generated),
        generated: c.expression.filter(|_| generated),
        generated_stored: c.generated == "s",
        identity: match c.identity.as_str() {
            "a" => Some(Identity::Always),
            "d" => Some(Identity::ByDefault),
            _ => None,
        },
        comment: c.comment,
        collation: c.collation.map(|(_, name)| name),
        on_update: None,
    }
}

pub(crate) async fn table(
    client: &Client,
    schema: &str,
    name: &str,
) -> Result<TableDef, DriverError> {
    let rel = client
        .query_opt(
            "SELECT c.oid, c.relpersistence::text, pg_get_partkeydef(c.oid), ts.spcname::text, \
                    array_to_string(c.reloptions, ', '), obj_description(c.oid, 'pg_class') \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_tablespace ts ON ts.oid = c.reltablespace \
             WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r', 'p')",
            &[&schema, &name],
        )
        .await
        .map_err(map_error)?
        .ok_or_else(|| not_found("table", &PgDialect.qualified(&table_ref(schema, name))))?;
    let oid: u32 = rel.get(0);
    let me = table_ref(schema, name);
    let mut def = TableDef::new(me.clone());
    def.columns = columns(client, oid)
        .await?
        .into_iter()
        .map(column_def)
        .collect();
    def.constraints = constraints(client, oid).await?;
    def.indexes = indexes(client, oid, false).await?;
    def.triggers = triggers(client, oid, &me).await?;
    def.comment = rel.get(5);
    if rel.get::<_, String>(1) == "u" {
        def.options.push(("unlogged".into(), "true".into()));
    }
    if let Some(key) = rel.get::<_, Option<String>>(2) {
        def.options.push(("partition_by".into(), key));
    }
    if let Some(with) = rel.get::<_, Option<String>>(4) {
        def.options.push(("with".into(), with));
    }
    if let Some(space) = rel.get::<_, Option<String>>(3) {
        def.options.push(("tablespace".into(), space));
    }
    def.partitions = client
        .query(
            "SELECT n.nspname::text, c.relname::text, pg_get_expr(c.relpartbound, c.oid) \
             FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE i.inhparent = $1 AND c.relispartition ORDER BY n.nspname, c.relname",
            &[&oid],
        )
        .await
        .map_err(map_error)?
        .iter()
        .map(|r| PartitionDef {
            table: table_ref(r.get(0), r.get(1)),
            bound: r.get(2),
        })
        .collect();
    Ok(def)
}

const CONSTRAINTS_SQL: &str = "\
SELECT con.conname::text, con.contype::text,
       ARRAY(SELECT a.attname::text FROM unnest(con.conkey) WITH ORDINALITY k(n, ord)
             JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.n
             ORDER BY k.ord),
       fn.nspname::text, fc.relname::text,
       ARRAY(SELECT a.attname::text FROM unnest(con.confkey) WITH ORDINALITY k(n, ord)
             JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.n
             ORDER BY k.ord),
       con.confupdtype::text, con.confdeltype::text, con.condeferrable,
       pg_get_expr(con.conbin, con.conrelid), con.condeferred,
       pg_get_constraintdef(con.oid),
       -- What the model cannot express: exclusion constraints, NOT VALID, NO INHERIT,
       -- MATCH FULL, deferrable keys, keys with INCLUDE or NULLS NOT DISTINCT.
       coalesce(con.contype = 'x' OR NOT con.convalidated
         OR (con.contype = 'c' AND con.connoinherit)
         OR (con.contype = 'f' AND con.confmatchtype = 'f')
         OR (con.contype IN ('p', 'u') AND (con.condeferrable
             OR i.indnkeyatts <> i.indnatts
             OR coalesce((to_jsonb(i) ->> 'indnullsnotdistinct')::bool, false))), false)
FROM pg_constraint con
LEFT JOIN pg_class fc ON fc.oid = con.confrelid
LEFT JOIN pg_namespace fn ON fn.oid = fc.relnamespace
LEFT JOIN pg_index i ON i.indexrelid = con.conindid AND con.contype IN ('p', 'u')
WHERE con.conrelid = $1 AND con.contype IN ('p', 'u', 'x', 'f', 'c')
ORDER BY CASE con.contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 WHEN 'x' THEN 1 WHEN 'f' THEN 2
         ELSE 3 END, con.conname";

fn fk_action(code: &str) -> FkAction {
    match code {
        "r" => FkAction::Restrict,
        "c" => FkAction::Cascade,
        "n" => FkAction::SetNull,
        "d" => FkAction::SetDefault,
        _ => FkAction::NoAction,
    }
}

/// A constraint from a row of [`CONSTRAINTS_SQL`] (its first thirteen columns).
pub(crate) fn constraint_from_row(r: &tokio_postgres::Row) -> ConstraintDef {
    let name = Some(r.get::<_, String>(0));
    let columns: Vec<String> = r.get(2);
    if r.get::<_, bool>(12) {
        return ConstraintDef::Raw {
            name,
            definition: r.get(11),
        };
    }
    match r.get::<_, String>(1).as_str() {
        "p" => ConstraintDef::PrimaryKey { name, columns },
        "u" => ConstraintDef::Unique { name, columns },
        "f" => ConstraintDef::ForeignKey {
            name,
            columns,
            target: table_ref(r.get(3), r.get(4)),
            target_columns: r.get(5),
            on_update: fk_action(r.get(6)),
            on_delete: fk_action(r.get(7)),
            deferrable: r.get(8),
            initially_deferred: r.get(10),
        },
        _ => ConstraintDef::Check {
            name,
            expression: r.get(9),
        },
    }
}

/// [`CONSTRAINTS_SQL`] for many tables at once: `$1` is an `oid[]` and each row ends with
/// its table's oid.
pub(crate) fn constraints_bulk_sql() -> String {
    CONSTRAINTS_SQL
        .replace("con.conrelid = $1", "con.conrelid = ANY($1)")
        .replace(
            "         OR (con.contype IN ('p', 'u') AND (con.condeferrable\n             OR i.indnkeyatts <> i.indnatts\n             OR coalesce((to_jsonb(i) ->> 'indnullsnotdistinct')::bool, false))), false)",
            "         OR (con.contype IN ('p', 'u') AND (con.condeferrable\n             OR i.indnkeyatts <> i.indnatts\n             OR coalesce((to_jsonb(i) ->> 'indnullsnotdistinct')::bool, false))), false),\n       con.conrelid",
        )
        .replace("ORDER BY CASE con.contype", "ORDER BY con.conrelid, CASE con.contype")
}

async fn constraints(client: &Client, oid: u32) -> Result<Vec<ConstraintDef>, DriverError> {
    let rows = client
        .query(CONSTRAINTS_SQL, &[&oid])
        .await
        .map_err(map_error)?;
    Ok(rows.iter().map(constraint_from_row).collect())
}

/// Columns of an index: key parts (names, or expressions in parentheses), then INCLUDE.
const INDEXES_SQL: &str = "\
SELECT ic.relname::text, i.indisunique, am.amname::text, i.indnkeyatts::int4,
       i.indoption::int2[], pg_get_expr(i.indpred, i.indrelid),
       ARRAY(SELECT coalesce(a.attname::text, pg_get_indexdef(i.indexrelid, u.ord::int4, true))
             FROM unnest(i.indkey::int2[]) WITH ORDINALITY u(n, ord)
             LEFT JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = u.n
             ORDER BY u.ord),
       i.indkey::int2[], pg_get_indexdef(i.indexrelid),
       -- What the model cannot express: operator classes other than the default, NULLS
       -- order other than the default, per-column collations, storage parameters and
       -- tablespaces.
       EXISTS (SELECT 1 FROM unnest(i.indclass::oid[]) c(opc)
               JOIN pg_opclass o ON o.oid = c.opc WHERE NOT o.opcdefault)
       OR EXISTS (SELECT 1 FROM unnest(i.indoption::int2[]) o(v)
                  WHERE ((v & 1) = 1) <> ((v & 2) = 2))
       OR EXISTS (SELECT 1 FROM unnest(i.indcollation::oid[], i.indkey::int2[]) k(coll, n)
                  JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.n
                  WHERE k.coll <> 0 AND k.coll <> a.attcollation)
       OR ic.reloptions IS NOT NULL OR ic.reltablespace <> 0
FROM pg_index i
JOIN pg_class ic ON ic.oid = i.indexrelid
JOIN pg_am am ON am.oid = ic.relam
WHERE i.indrelid = $1 AND (ic.relname = $2 OR $2 IS NULL)
  AND ($3 OR NOT EXISTS (
      SELECT 1 FROM pg_constraint c WHERE c.conindid = i.indexrelid
      AND c.conrelid = i.indrelid AND c.contype IN ('p', 'u', 'x')))
ORDER BY ic.relname";

async fn index_defs(
    client: &Client,
    oid: u32,
    name: Option<&str>,
    with_constraints: bool,
) -> Result<Vec<IndexDef>, DriverError> {
    let rows = client
        .query(INDEXES_SQL, &[&oid, &name, &with_constraints])
        .await
        .map_err(map_error)?;
    Ok(rows.iter().map(index_from_row).collect())
}

/// An index from a row of [`INDEXES_SQL`] (its first ten columns).
pub(crate) fn index_from_row(r: &tokio_postgres::Row) -> IndexDef {
    let key_count = r.get::<_, i32>(3) as usize;
    let options: Vec<i16> = r.get(4);
    let parts: Vec<String> = r.get(6);
    let attnums: Vec<i16> = r.get(7);
    let columns = parts
        .iter()
        .zip(&attnums)
        .take(key_count)
        .enumerate()
        .map(|(k, (part, attnum))| IndexColumn {
            expression: *attnum == 0,
            expr: if *attnum == 0 {
                format!("({part})")
            } else {
                part.clone()
            },
            desc: options.get(k).is_some_and(|o| o & 1 == 1),
            prefix: None,
        })
        .collect();
    IndexDef {
        name: r.get(0),
        columns,
        unique: r.get(1),
        method: Some(r.get(2)),
        predicate: r.get(5),
        include: parts.iter().skip(key_count).cloned().collect(),
        definition: r.get::<_, bool>(9).then(|| r.get(8)),
    }
}

/// [`INDEXES_SQL`] for many relations at once: `$1` is an `oid[]`, `$2` stays `NULL`, `$3`
/// keeps indexes backing a constraint; each row ends with its relation's oid.
pub(crate) fn indexes_bulk_sql() -> String {
    INDEXES_SQL
        .replace("i.indrelid = $1", "i.indrelid = ANY($1)")
        .replace(
            "OR ic.reloptions IS NOT NULL OR ic.reltablespace <> 0\nFROM pg_index i",
            "OR ic.reloptions IS NOT NULL OR ic.reltablespace <> 0,\n       i.indrelid\nFROM pg_index i",
        )
        .replace("ORDER BY ic.relname", "ORDER BY i.indrelid, ic.relname")
}

/// Indexes of a relation; `with_constraints` keeps those backing a constraint.
pub(crate) async fn indexes(
    client: &Client,
    oid: u32,
    with_constraints: bool,
) -> Result<Vec<IndexDef>, DriverError> {
    index_defs(client, oid, None, with_constraints).await
}

async fn index(client: &Client, schema: &str, name: &str) -> Result<ObjectDef, DriverError> {
    let row = client
        .query_opt(
            "SELECT i.indrelid, tn.nspname::text, t.relname::text FROM pg_index i \
             JOIN pg_class ic ON ic.oid = i.indexrelid \
             JOIN pg_namespace n ON n.oid = ic.relnamespace \
             JOIN pg_class t ON t.oid = i.indrelid \
             JOIN pg_namespace tn ON tn.oid = t.relnamespace \
             WHERE n.nspname = $1 AND ic.relname = $2",
            &[&schema, &name],
        )
        .await
        .map_err(map_error)?
        .ok_or_else(|| not_found("index", name))?;
    let oid: u32 = row.get(0);
    let index = index_defs(client, oid, Some(name), true)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| not_found("index", name))?;
    Ok(ObjectDef::Index {
        table: table_ref(row.get(1), row.get(2)),
        index,
    })
}

/// `tgtype` bits (`pg_trigger.h`).
const TRIGGER_ROW: i16 = 1 << 0;
const TRIGGER_BEFORE: i16 = 1 << 1;
const TRIGGER_INSERT: i16 = 1 << 2;
const TRIGGER_DELETE: i16 = 1 << 3;
const TRIGGER_UPDATE: i16 = 1 << 4;
const TRIGGER_TRUNCATE: i16 = 1 << 5;
const TRIGGER_INSTEAD: i16 = 1 << 6;

/// The `WHEN` condition and the function call of `pg_get_triggerdef`'s text.
fn split_triggerdef(def: &str) -> (Option<String>, String) {
    const EXECUTE: &str = " EXECUTE FUNCTION ";
    let Some(at) = def.rfind(EXECUTE) else {
        return (None, String::new());
    };
    let body = def[at + EXECUTE.len()..].to_string();
    let head = &def[..at];
    let condition = [" FOR EACH ROW WHEN (", " FOR EACH STATEMENT WHEN ("]
        .iter()
        .find_map(|marker| head.rfind(marker).map(|p| p + marker.len()))
        .and_then(|start| head[start..].strip_suffix(')'))
        .map(String::from);
    (condition, body)
}

async fn triggers(
    client: &Client,
    oid: u32,
    table: &TableRef,
) -> Result<Vec<TriggerDef>, DriverError> {
    let rows = client
        .query(TRIGGERS_SQL, &[&vec![oid]])
        .await
        .map_err(map_error)?;
    Ok(rows.iter().map(|r| trigger_from_row(r, table)).collect())
}

/// The triggers of many tables at once (`$1` is an `oid[]`); each row ends with its table's
/// oid.
pub(crate) const TRIGGERS_SQL: &str = "\
SELECT t.tgname::text, t.tgtype, pg_get_triggerdef(t.oid),
       ARRAY(SELECT a.attname::text FROM unnest(t.tgattr::int2[]) WITH ORDINALITY k(n, ord)
             JOIN pg_attribute a ON a.attrelid = t.tgrelid AND a.attnum = k.n
             ORDER BY k.ord),
       t.tgrelid
FROM pg_trigger t WHERE t.tgrelid = ANY($1) AND NOT t.tgisinternal
ORDER BY t.tgrelid, t.tgname";

/// A trigger of `table` from a row of [`TRIGGERS_SQL`].
pub(crate) fn trigger_from_row(r: &tokio_postgres::Row, table: &TableRef) -> TriggerDef {
    {
        {
            let ty: i16 = r.get(1);
            let def: String = r.get(2);
            let update_of: Vec<String> = r.get(3);
            let timing = if ty & TRIGGER_INSTEAD != 0 {
                "INSTEAD OF"
            } else if ty & TRIGGER_BEFORE != 0 {
                "BEFORE"
            } else {
                "AFTER"
            };
            let mut events = Vec::new();
            if ty & TRIGGER_INSERT != 0 {
                events.push("INSERT".to_string());
            }
            if ty & TRIGGER_DELETE != 0 {
                events.push("DELETE".to_string());
            }
            if ty & TRIGGER_UPDATE != 0 {
                if update_of.is_empty() {
                    events.push("UPDATE".to_string());
                } else {
                    let cols: Vec<String> =
                        update_of.iter().map(|c| PgDialect.quote_ident(c)).collect();
                    events.push(format!("UPDATE OF {}", cols.join(", ")));
                }
            }
            if ty & TRIGGER_TRUNCATE != 0 {
                events.push("TRUNCATE".to_string());
            }
            let (condition, body) = split_triggerdef(&def);
            TriggerDef {
                name: r.get(0),
                table: table.clone(),
                timing: timing.to_string(),
                events,
                for_each_row: ty & TRIGGER_ROW != 0,
                condition,
                body,
            }
        }
    }
}

async fn view(client: &Client, schema: &str, name: &str) -> Result<ObjectDef, DriverError> {
    let row = client
        .query_opt(
            "SELECT c.oid, c.relkind::text, pg_get_viewdef(c.oid, true), \
                    obj_description(c.oid, 'pg_class'), array_to_string(c.reloptions, ', '), \
                    ts.spcname::text \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_tablespace ts ON ts.oid = c.reltablespace \
             WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('v', 'm')",
            &[&schema, &name],
        )
        .await
        .map_err(map_error)?
        .ok_or_else(|| not_found("view", &PgDialect.qualified(&table_ref(schema, name))))?;
    let oid: u32 = row.get(0);
    let query: String = row.get(2);
    let materialized = row.get::<_, String>(1) == "m";
    let mut options = Vec::new();
    if let Some(with) = row.get::<_, Option<String>>(4) {
        options.push(("with".to_string(), with));
    }
    if let Some(space) = row.get::<_, Option<String>>(5) {
        options.push(("tablespace".to_string(), space));
    }
    Ok(ObjectDef::View(ViewDef {
        view: table_ref(schema, name),
        query: query.trim().trim_end_matches(';').to_string(),
        materialized,
        columns: columns(client, oid)
            .await?
            .into_iter()
            .map(|c| c.name)
            .collect(),
        comment: row.get(3),
        options,
        indexes: if materialized {
            indexes(client, oid, true).await?
        } else {
            Vec::new()
        },
    }))
}

async fn sequence(client: &Client, schema: &str, name: &str) -> Result<ObjectDef, DriverError> {
    let s = client
        .query_opt(
            "SELECT format_type(s.seqtypid, NULL), s.seqincrement, s.seqmin, s.seqmax, \
                    s.seqstart, s.seqcache, s.seqcycle, obj_description(c.oid, 'pg_class') \
             FROM pg_sequence s JOIN pg_class c ON c.oid = s.seqrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2",
            &[&schema, &name],
        )
        .await
        .map_err(map_error)?
        .ok_or_else(|| not_found("sequence", &PgDialect.qualified(&table_ref(schema, name))))?;
    Ok(ObjectDef::Sequence(SequenceDef {
        sequence: table_ref(schema, name),
        data_type: Some(s.get(0)),
        increment: Some(s.get(1)),
        min_value: Some(s.get(2)),
        max_value: Some(s.get(3)),
        start: Some(s.get(4)),
        cache: Some(s.get(5)),
        cycle: s.get(6),
        comment: s.get(7),
    }))
}

/// A function or procedure; `signature` (identity arguments) picks one of an overloaded
/// name.
async fn routine(
    client: &Client,
    schema: &str,
    name: &str,
    signature: Option<&str>,
) -> Result<ObjectDef, DriverError> {
    let rows = client
        .query(
            "SELECT p.prokind::text, pg_get_function_arguments(p.oid), \
                    CASE WHEN p.prokind = 'p' THEN NULL ELSE pg_get_function_result(p.oid) END, \
                    l.lanname::text, \
                    CASE WHEN p.prosqlbody IS NULL THEN p.prosrc \
                         ELSE pg_get_function_sqlbody(p.oid) END, \
                    p.provolatile::text, p.proisstrict, p.prosecdef, p.proparallel::text, \
                    p.prosqlbody IS NOT NULL, pg_get_function_identity_arguments(p.oid) \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             JOIN pg_language l ON l.oid = p.prolang \
             WHERE n.nspname = $1 AND p.proname = $2 \
               AND ($3::text IS NULL OR pg_get_function_identity_arguments(p.oid) = $3) \
             ORDER BY p.oid",
            &[&schema, &name, &signature],
        )
        .await
        .map_err(map_error)?;
    let r = match rows.as_slice() {
        [r] => r,
        [] => return Err(not_found("function", name)),
        _ => {
            return Err(DriverError::new(
                ErrorKind::Other,
                format!("function {name} is overloaded: name its arguments"),
            ));
        }
    };
    Ok(ObjectDef::Routine(routine_from_row(r, schema, name)))
}

/// A routine from a row of the routine queries (their first eleven columns).
pub(crate) fn routine_from_row(r: &tokio_postgres::Row, schema: &str, name: &str) -> RoutineDef {
    let kind = if r.get::<_, String>(0) == "p" {
        ObjectKind::Procedure
    } else {
        ObjectKind::Function
    };
    let mut options = Vec::new();
    if kind == ObjectKind::Function {
        let volatility = match r.get::<_, String>(5).as_str() {
            "i" => "IMMUTABLE",
            "s" => "STABLE",
            _ => "VOLATILE",
        };
        options.push(("volatility".to_string(), volatility.to_string()));
        if r.get(6) {
            options.push(("strict".into(), "true".into()));
        }
    }
    if r.get(7) {
        options.push(("security".into(), "DEFINER".into()));
    }
    match r.get::<_, String>(8).as_str() {
        "s" => options.push(("parallel".into(), "SAFE".into())),
        "r" => options.push(("parallel".into(), "RESTRICTED".into())),
        _ => {}
    }
    if r.get(9) {
        options.push(("sql_body".into(), "true".into()));
    }
    RoutineDef {
        schema: Some(schema.to_string()),
        name: name.to_string(),
        kind,
        arguments: r.get(1),
        identity_arguments: Some(r.get(10)),
        returns: r.get(2),
        language: Some(r.get(3)),
        body: r.get(4),
        options,
    }
}

/// Every function and procedure of a schema, each row ending with its name.
pub(crate) const ROUTINES_SQL: &str = "\
SELECT p.prokind::text, pg_get_function_arguments(p.oid),
       CASE WHEN p.prokind = 'p' THEN NULL ELSE pg_get_function_result(p.oid) END,
       l.lanname::text,
       CASE WHEN p.prosqlbody IS NULL THEN p.prosrc ELSE pg_get_function_sqlbody(p.oid) END,
       p.provolatile::text, p.proisstrict, p.prosecdef, p.proparallel::text,
       p.prosqlbody IS NOT NULL, pg_get_function_identity_arguments(p.oid), p.proname::text
FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
JOIN pg_language l ON l.oid = p.prolang
WHERE n.nspname = $1 AND p.prokind IN ('f', 'p')
ORDER BY p.proname, p.oid";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_queries_are_derived_from_the_single_object_ones() {
        // Each rewrite must have taken effect: the relation oid is the last column and the
        // filter takes an oid array.
        let columns = crate::describe::columns_bulk_sql();
        assert!(columns.contains("a.attrelid = ANY($1)"), "{columns}");
        assert!(
            columns.contains("a.attrelid\nFROM pg_attribute a"),
            "{columns}"
        );
        assert!(
            columns.contains("ORDER BY a.attrelid, a.attnum"),
            "{columns}"
        );
        let constraints = constraints_bulk_sql();
        assert!(
            constraints.contains("con.conrelid = ANY($1)"),
            "{constraints}"
        );
        assert!(
            constraints.contains("con.conrelid\nFROM pg_constraint con"),
            "{constraints}"
        );
        assert!(
            constraints.contains("ORDER BY con.conrelid, CASE"),
            "{constraints}"
        );
        let indexes = indexes_bulk_sql();
        assert!(indexes.contains("i.indrelid = ANY($1)"), "{indexes}");
        assert!(indexes.contains("i.indrelid\nFROM pg_index i"), "{indexes}");
        assert!(
            indexes.contains("ORDER BY i.indrelid, ic.relname"),
            "{indexes}"
        );
    }

    #[test]
    fn trigger_condition_and_body() {
        let def = "CREATE TRIGGER t BEFORE INSERT ON s.t FOR EACH ROW \
                   WHEN ((new.x > 0)) EXECUTE FUNCTION s.f('a')";
        assert_eq!(
            split_triggerdef(def),
            (Some("(new.x > 0)".into()), "s.f('a')".into())
        );
        let def = "CREATE TRIGGER t AFTER DELETE ON t FOR EACH STATEMENT EXECUTE FUNCTION f()";
        assert_eq!(split_triggerdef(def), (None, "f()".into()));
    }
}
