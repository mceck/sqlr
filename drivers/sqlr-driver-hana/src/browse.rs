//! The explorer tree: schemas → object folders → objects → columns. Catalog queries are
//! parameterized; names are never spliced into SQL.

use hdbconnect_async::{Connection as HdbConnection, HdbValue};
use sqlr_driver_sdk::{DriverError, Node, NodeKind, NodePath};

use crate::errors;

/// Runs `sql` with string `params`, returning the string columns of each row.
pub(crate) async fn rows(
    conn: &HdbConnection,
    sql: &str,
    params: &[&str],
) -> Result<Vec<Vec<String>>, DriverError> {
    let mut statement = conn.prepare(sql).await.map_err(errors::map)?;
    let values: Vec<HdbValue<'_>> = params
        .iter()
        .map(|p| HdbValue::STRING(p.to_string()))
        .collect();
    let mut rs = statement
        .execute_row(values)
        .await
        .map_err(errors::map)?
        .into_result_set()
        .map_err(errors::map)?;
    let mut out = Vec::new();
    while let Some(mut row) = rs.next_row().await.map_err(errors::map)? {
        let mut cells = Vec::with_capacity(row.len());
        for _ in 0..row.len() {
            cells.push(
                row.next_try_into::<Option<String>>()
                    .map_err(errors::map)?
                    .unwrap_or_default(),
            );
        }
        out.push(cells);
    }
    Ok(out)
}

const FOLDERS: [(&str, &str, &str, &str); 5] = [
    ("tables", "Tables", "SYS.TABLES", "TABLE_NAME"),
    ("views", "Views", "SYS.VIEWS", "VIEW_NAME"),
    (
        "procedures",
        "Procedures",
        "SYS.PROCEDURES",
        "PROCEDURE_NAME",
    ),
    ("functions", "Functions", "SYS.FUNCTIONS", "FUNCTION_NAME"),
    ("sequences", "Sequences", "SYS.SEQUENCES", "SEQUENCE_NAME"),
];

fn node(
    path: NodePath,
    name: &str,
    kind: NodeKind,
    has_children: bool,
    detail: Option<String>,
) -> Node {
    Node {
        path,
        name: name.to_string(),
        kind,
        has_children,
        detail,
    }
}

pub(crate) async fn browse(
    conn: &HdbConnection,
    path: &NodePath,
) -> Result<Vec<Node>, DriverError> {
    let segments = path.segments();
    match segments {
        [] => {
            let schemas = rows(
                conn,
                "SELECT SCHEMA_NAME FROM SYS.SCHEMAS ORDER BY SCHEMA_NAME",
                &[],
            )
            .await?;
            Ok(schemas
                .into_iter()
                .map(|r| node(path.child(&r[0]), &r[0], NodeKind::Schema, true, None))
                .collect())
        }
        [_schema] => Ok(FOLDERS
            .iter()
            .map(|(key, label, _, _)| node(path.child(key), label, NodeKind::Folder, true, None))
            .collect()),
        [schema, folder] => {
            let Some((_, _, view, column)) = FOLDERS.iter().find(|f| f.0 == folder) else {
                return Ok(Vec::new());
            };
            // `view` and `column` come from the constant table above, never from the user.
            let sql =
                format!("SELECT {column} FROM {view} WHERE SCHEMA_NAME = ? ORDER BY {column}");
            let kind = match folder.as_str() {
                "tables" => NodeKind::Table,
                "views" => NodeKind::View,
                "procedures" => NodeKind::Procedure,
                "functions" => NodeKind::Function,
                _ => NodeKind::Sequence,
            };
            let opens = matches!(folder.as_str(), "tables" | "views");
            let names = rows(conn, &sql, &[schema]).await?;
            Ok(names
                .into_iter()
                .map(|r| node(path.child(&r[0]), &r[0], kind, opens, None))
                .collect())
        }
        [schema, folder, name] if folder == "tables" || folder == "views" => {
            let sql = if folder == "tables" {
                "SELECT COLUMN_NAME, DATA_TYPE_NAME FROM SYS.TABLE_COLUMNS \
                 WHERE SCHEMA_NAME = ? AND TABLE_NAME = ? ORDER BY POSITION"
            } else {
                "SELECT COLUMN_NAME, DATA_TYPE_NAME FROM SYS.VIEW_COLUMNS \
                 WHERE SCHEMA_NAME = ? AND VIEW_NAME = ? ORDER BY POSITION"
            };
            let columns = rows(conn, sql, &[schema, name]).await?;
            Ok(columns
                .into_iter()
                .map(|r| {
                    node(
                        path.child(&r[0]),
                        &r[0],
                        NodeKind::Column,
                        false,
                        Some(r[1].clone()),
                    )
                })
                .collect())
        }
        _ => Ok(Vec::new()),
    }
}
