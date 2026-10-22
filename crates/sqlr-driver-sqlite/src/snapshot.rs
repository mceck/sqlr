//! A whole schema in one statement: every definition from `sqlite_schema`, parsed with the
//! same parsers as a single object. The cost does not grow with the number of tables.

use std::collections::BTreeMap;

use rusqlite::Connection;
use sqlr_core::{
    DependentDef, Dialect, DriverError, ObjectDef, ObjectKind, SchemaSnapshot, TableRef,
};

use crate::dialect::SqliteDialect;
use crate::introspect::{names_table, parse_index, parse_table, parse_trigger, parse_view};
use crate::map_error;

struct Row {
    ty: String,
    name: String,
    table: String,
    sql: String,
}

pub(crate) fn load_schema(conn: &Connection, schema: &str) -> Result<SchemaSnapshot, DriverError> {
    let q = SqliteDialect.quote_ident(schema);
    // The creation order (rowid) is the order dependents are recreated in.
    let rows: Vec<Row> = {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT type, name, tbl_name, sql FROM {q}.sqlite_schema \
                 WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
                 ORDER BY rowid"
            ))
            .map_err(map_error)?;
        stmt.query_map([], |r| {
            Ok(Row {
                ty: r.get(0)?,
                name: r.get(1)?,
                table: r.get(2)?,
                sql: r.get(3)?,
            })
        })
        .map_err(map_error)?
        .collect::<Result<_, _>>()
        .map_err(map_error)?
    };
    let mut indexes: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    let mut triggers: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for row in &rows {
        match row.ty.as_str() {
            "index" => indexes.entry(row.table.as_str()).or_default().push(row),
            "trigger" => triggers.entry(row.table.as_str()).or_default().push(row),
            _ => {}
        }
    }
    let by_name = |rows: &mut Vec<&Row>| rows.sort_by(|a, b| a.name.cmp(&b.name));

    let mut objects = Vec::new();
    for row in &rows {
        match row.ty.as_str() {
            "table"
                if row
                    .sql
                    .trim_start()
                    .to_ascii_uppercase()
                    .starts_with("CREATE VIRTUAL") => {}
            "table" => {
                let mut def = parse_table(&row.sql, schema)?;
                def.table = TableRef {
                    database: None,
                    schema: Some(schema.to_string()),
                    name: row.name.clone(),
                };
                let mut own = indexes.get(row.name.as_str()).cloned().unwrap_or_default();
                by_name(&mut own);
                def.indexes = own
                    .iter()
                    .map(|r| parse_index(&r.sql))
                    .collect::<Result<_, _>>()?;
                let mut own = triggers.get(row.name.as_str()).cloned().unwrap_or_default();
                by_name(&mut own);
                def.triggers = own
                    .iter()
                    .map(|r| parse_trigger(&r.sql, schema))
                    .collect::<Result<_, _>>()?;
                // Views, and triggers of other tables, that name the table.
                def.dependents = rows
                    .iter()
                    .filter(|r| {
                        (r.ty == "view" || (r.ty == "trigger" && r.table != row.name))
                            && !r.name.eq_ignore_ascii_case(&row.name)
                            && names_table(&r.sql, &row.name)
                    })
                    .map(|r| DependentDef {
                        kind: if r.ty == "view" {
                            ObjectKind::View
                        } else {
                            ObjectKind::Trigger
                        },
                        name: r.name.clone(),
                        sql: r.sql.clone(),
                    })
                    .collect();
                objects.push(ObjectDef::Table(def));
            }
            "view" => {
                let mut view = parse_view(&row.sql, schema)?;
                view.view.name = row.name.clone();
                // The columns a view yields would need one pragma per view; a compare has
                // no use for them, so only the ones its SQL names are kept.
                objects.push(ObjectDef::View(view));
            }
            _ => {}
        }
    }
    Ok(SchemaSnapshot {
        engine: "sqlite".into(),
        schema: schema.to_string(),
        objects,
        dependencies: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static STATEMENTS: AtomicUsize = AtomicUsize::new(0);

    fn count(_sql: &str, _took: std::time::Duration) {
        STATEMENTS.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn compare_bulk_snapshot_query_count() {
        let conn = Connection::open_in_memory().unwrap();
        let mut ddl = String::new();
        for i in 0..400 {
            ddl.push_str(&format!(
                "CREATE TABLE t{i} (id INTEGER PRIMARY KEY, parent INTEGER REFERENCES t0(id), \
                 name TEXT); CREATE INDEX i{i} ON t{i}(name); \
                 CREATE VIEW v{i} AS SELECT id FROM t{i};"
            ));
        }
        conn.execute_batch(&ddl).unwrap();
        let mut conn = conn;
        #[allow(deprecated)] // the statement counter is all the test needs
        conn.profile(Some(count));
        STATEMENTS.store(0, Ordering::SeqCst);
        let snapshot = load_schema(&conn, "main").unwrap();
        let statements = STATEMENTS.load(Ordering::SeqCst);
        assert_eq!(snapshot.objects.len(), 800);
        assert!(
            statements <= 15,
            "{statements} statements for 800 objects: the count must not follow their number"
        );
    }
}
