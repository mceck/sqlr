//! Explorer tree backed by Oracle's `ALL_*` data dictionary views.

use oracle_rs::{Connection, QueryResult, Value as OracleValue};
use sqlr_core::{DriverError, ErrorKind, Node, NodeKind, NodePath};

use crate::map_error;

const FOLDERS: [&str; 6] = [
    "tables",
    "views",
    "sequences",
    "functions",
    "procedures",
    "triggers",
];

fn node(
    parent: &NodePath,
    name: String,
    kind: NodeKind,
    has_children: bool,
    detail: Option<String>,
) -> Node {
    Node {
        path: parent.child(&name),
        name,
        kind,
        has_children,
        detail,
    }
}

fn unsupported(path: &NodePath) -> DriverError {
    DriverError::new(
        ErrorKind::Unsupported,
        format!("cannot browse {:?}", path.segments()),
    )
}

/// Runs a catalog query with string binds.
pub(crate) async fn query(
    conn: &Connection,
    sql: &str,
    params: &[&str],
) -> Result<QueryResult, DriverError> {
    let values: Vec<OracleValue> = params
        .iter()
        .map(|v| OracleValue::String((*v).to_string()))
        .collect();
    conn.query(sql, &values).await.map_err(map_error)
}

/// A dictionary number (a length, a precision): the server may send a NUMBER as text.
pub(crate) fn int(row: &oracle_rs::Row, index: usize) -> Option<i64> {
    let value = row.get(index)?;
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
}

fn text(row: &oracle_rs::Row, index: usize) -> Option<String> {
    row.get(index)
        .and_then(OracleValue::as_str)
        .map(String::from)
}

pub(crate) async fn browse(conn: &Connection, path: &NodePath) -> Result<Vec<Node>, DriverError> {
    let segs: Vec<&str> = path.segments().iter().map(String::as_str).collect();
    match segs.as_slice() {
        [] => schemas(conn, path).await,
        [_] => Ok(FOLDERS
            .iter()
            .map(|f| node(path, (*f).to_string(), NodeKind::Folder, true, None))
            .collect()),
        [s, "tables"] => tables(conn, path, s).await,
        [s, "views"] => views(conn, path, s).await,
        [s, "sequences"] => sequences(conn, path, s).await,
        [s, "functions"] => routines(conn, path, s, "FUNCTION", NodeKind::Function).await,
        [s, "procedures"] => routines(conn, path, s, "PROCEDURE", NodeKind::Procedure).await,
        [s, "triggers"] => triggers(conn, path, s, None).await,
        [_, "tables", _] => Ok(["columns", "indexes", "triggers"]
            .iter()
            .map(|f| node(path, (*f).to_string(), NodeKind::Folder, true, None))
            .collect()),
        [s, "tables", t, "columns"] => columns(conn, path, s, t).await,
        [s, "tables", t, "indexes"] => indexes(conn, path, s, t).await,
        [s, "tables", t, "triggers"] => triggers(conn, path, s, Some(t)).await,
        [s, "views", v] => columns(conn, path, s, v).await,
        _ => Err(unsupported(path)),
    }
}

async fn schemas(conn: &Connection, path: &NodePath) -> Result<Vec<Node>, DriverError> {
    let result = query(
        conn,
        "SELECT u.username FROM all_users u \
         WHERE u.username = SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA') \
            OR EXISTS (SELECT 1 FROM all_objects o WHERE o.owner = u.username \
                       AND o.object_type IN ('TABLE', 'VIEW', 'SEQUENCE', 'PROCEDURE', 'FUNCTION')) \
         ORDER BY CASE WHEN u.username = SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA') THEN 0 ELSE 1 END, \
                  u.username",
        &[],
    )
    .await?;
    Ok(result
        .rows
        .iter()
        .filter_map(|r| text(r, 0))
        .map(|name| node(path, name, NodeKind::Schema, true, None))
        .collect())
}

async fn tables(
    conn: &Connection,
    path: &NodePath,
    schema: &str,
) -> Result<Vec<Node>, DriverError> {
    let result = query(
        conn,
        "SELECT table_name FROM all_tables WHERE owner = :1 ORDER BY table_name",
        &[schema],
    )
    .await?;
    Ok(result
        .rows
        .iter()
        .filter_map(|r| text(r, 0))
        .map(|name| node(path, name, NodeKind::Table, true, None))
        .collect())
}

async fn views(conn: &Connection, path: &NodePath, schema: &str) -> Result<Vec<Node>, DriverError> {
    let result = query(
        conn,
        "SELECT view_name FROM all_views WHERE owner = :1 ORDER BY view_name",
        &[schema],
    )
    .await?;
    Ok(result
        .rows
        .iter()
        .filter_map(|r| text(r, 0))
        .map(|name| node(path, name, NodeKind::View, true, None))
        .collect())
}

async fn sequences(
    conn: &Connection,
    path: &NodePath,
    schema: &str,
) -> Result<Vec<Node>, DriverError> {
    let result = query(
        conn,
        "SELECT sequence_name FROM all_sequences WHERE sequence_owner = :1 ORDER BY sequence_name",
        &[schema],
    )
    .await?;
    Ok(result
        .rows
        .iter()
        .filter_map(|r| text(r, 0))
        .map(|name| node(path, name, NodeKind::Sequence, false, None))
        .collect())
}

async fn routines(
    conn: &Connection,
    path: &NodePath,
    schema: &str,
    object_type: &str,
    kind: NodeKind,
) -> Result<Vec<Node>, DriverError> {
    let result = query(
        conn,
        "SELECT object_name FROM all_objects \
         WHERE owner = :1 AND object_type = :2 AND status = 'VALID' ORDER BY object_name",
        &[schema, object_type],
    )
    .await?;
    Ok(result
        .rows
        .iter()
        .filter_map(|r| text(r, 0))
        .map(|name| node(path, name, kind, false, None))
        .collect())
}

async fn triggers(
    conn: &Connection,
    path: &NodePath,
    schema: &str,
    table: Option<&str>,
) -> Result<Vec<Node>, DriverError> {
    let result = match table {
        Some(table) => {
            query(
                conn,
                "SELECT trigger_name, status FROM all_triggers \
                 WHERE table_owner = :1 AND table_name = :2 ORDER BY trigger_name",
                &[schema, table],
            )
            .await?
        }
        None => {
            query(
                conn,
                "SELECT trigger_name, status FROM all_triggers \
                 WHERE owner = :1 ORDER BY trigger_name",
                &[schema],
            )
            .await?
        }
    };
    Ok(result
        .rows
        .iter()
        .filter_map(|r| {
            let name = text(r, 0)?;
            let status = text(r, 1).filter(|s| s != "ENABLED");
            Some(node(path, name, NodeKind::Trigger, false, status))
        })
        .collect())
}

/// A declared type with the length or precision the column carries.
pub(crate) fn type_name(
    data_type: &str,
    char_length: Option<i64>,
    data_length: Option<i64>,
    precision: Option<i64>,
    scale: Option<i64>,
) -> String {
    match data_type {
        "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR" => {
            let length = char_length.or(data_length).unwrap_or(0);
            format!("{data_type}({length})")
        }
        "RAW" => format!("RAW({})", data_length.unwrap_or(0)),
        "NUMBER" | "DECIMAL" | "NUMERIC" => match precision {
            Some(p) if p > 0 => match scale {
                Some(s) if s > 0 => format!("NUMBER({p},{s})"),
                Some(s) if s < 0 => format!("NUMBER({p},{s})"),
                _ => format!("NUMBER({p})"),
            },
            _ => "NUMBER".to_string(),
        },
        "FLOAT" => match precision {
            Some(p) if p > 0 => format!("FLOAT({p})"),
            _ => "FLOAT".to_string(),
        },
        other => other.to_string(),
    }
}

/// Columns of one table or view, for browsing and descriptions.
pub(crate) async fn column_defs(
    conn: &Connection,
    schema: &str,
    relation: &str,
) -> Result<Vec<sqlr_core::TableColumn>, DriverError> {
    let result = query(
        conn,
        "SELECT c.column_name, c.data_type, c.char_length, c.data_length, c.data_precision, \
                c.data_scale, c.nullable, c.identity_column, c.virtual_column \
         FROM all_tab_cols c \
         WHERE c.owner = :1 AND c.table_name = :2 AND c.hidden_column = 'NO' \
         ORDER BY c.column_id",
        &[schema, relation],
    )
    .await?;
    // `data_default` is a LONG; oracle-rs returns no rows when a LONG is selected with
    // binds, so read it separately with a quoted predicate.
    let quote = |value: &str| value.replace('\'', "''");
    let defaults = query(
        conn,
        &format!(
            "SELECT column_name, data_default FROM all_tab_cols \
             WHERE owner = '{}' AND table_name = '{}' AND hidden_column = 'NO'",
            quote(schema),
            quote(relation)
        ),
        &[],
    )
    .await?;
    let defaults: std::collections::BTreeMap<String, Option<String>> = defaults
        .rows
        .iter()
        .map(|row| {
            (
                text(row, 0).unwrap_or_default(),
                text(row, 1).map(|s| s.trim().to_string()),
            )
        })
        .collect();
    Ok(result
        .rows
        .iter()
        .map(|row| {
            let data_type = text(row, 1).unwrap_or_else(|| "UNKNOWN".to_string());
            let generated =
                text(row, 7).as_deref() == Some("YES") || text(row, 8).as_deref() == Some("YES");
            let name = text(row, 0).unwrap_or_default();
            sqlr_core::TableColumn {
                default: defaults.get(&name).cloned().flatten(),
                name,
                type_name: type_name(
                    &data_type,
                    int(row, 2),
                    int(row, 3),
                    int(row, 4),
                    int(row, 5),
                ),
                nullable: text(row, 6).as_deref() == Some("Y"),
                generated,
            }
        })
        .collect())
}

async fn columns(
    conn: &Connection,
    path: &NodePath,
    schema: &str,
    relation: &str,
) -> Result<Vec<Node>, DriverError> {
    Ok(column_defs(conn, schema, relation)
        .await?
        .into_iter()
        .map(|c| {
            let detail = if c.nullable {
                c.type_name
            } else {
                format!("{} NOT NULL", c.type_name)
            };
            node(path, c.name, NodeKind::Column, false, Some(detail))
        })
        .collect())
}

async fn indexes(
    conn: &Connection,
    path: &NodePath,
    schema: &str,
    table: &str,
) -> Result<Vec<Node>, DriverError> {
    let result = query(
        conn,
        "SELECT index_name, index_type, uniqueness FROM all_indexes \
         WHERE table_owner = :1 AND table_name = :2 ORDER BY index_name",
        &[schema, table],
    )
    .await?;
    Ok(result
        .rows
        .iter()
        .filter_map(|r| {
            let name = text(r, 0)?;
            let kind = text(r, 1).unwrap_or_default();
            let unique = text(r, 2).unwrap_or_default();
            Some(node(
                path,
                name,
                NodeKind::Index,
                false,
                Some(format!("{kind} {unique}")),
            ))
        })
        .collect())
}
