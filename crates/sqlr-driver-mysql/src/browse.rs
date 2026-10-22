//! Explorer tree backed by `information_schema`. All catalog queries are parameterized.
//!
//! MySQL databases are its schemas: the root lists them, with the server's own
//! (`information_schema`, `mysql`, `performance_schema`, `sys`) under a "System" folder.
//! Paths are `[database, folder, object, …]` wherever the database is listed; functions and
//! procedures have separate folders because they may share a name.

use mysql_async::Conn;
use sqlr_core::{DriverError, ErrorKind, Node, NodeKind, NodePath};

use crate::catalog::{int, rows, string, text};

/// Path segment of the "System" folder. A NUL is never part of a database name, so it
/// cannot collide with one.
pub(crate) const SYSTEM_FOLDER: &str = "\u{0}system";

const SYSTEM_SCHEMAS: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

const FOLDERS: [&str; 5] = ["tables", "views", "functions", "procedures", "triggers"];

fn is_system(schema: &str) -> bool {
    SYSTEM_SCHEMAS
        .iter()
        .any(|s| s.eq_ignore_ascii_case(schema))
}

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

fn folders(path: &NodePath, names: &[&str]) -> Vec<Node> {
    names
        .iter()
        .map(|f| node(path, f.to_string(), NodeKind::Folder, true, None))
        .collect()
}

fn unsupported(path: &NodePath) -> DriverError {
    DriverError::new(
        ErrorKind::Unsupported,
        format!("cannot browse {:?}", path.segments()),
    )
}

/// `current` is the connection's database; without `show_all` only it (and the System
/// folder) is listed at the root.
pub(crate) async fn browse(
    conn: &mut Conn,
    path: &NodePath,
    current: Option<&str>,
    show_all: bool,
) -> Result<Vec<Node>, DriverError> {
    let segs: Vec<&str> = path.segments().iter().map(String::as_str).collect();
    match segs.as_slice() {
        [] => root(conn, current, show_all).await,
        [SYSTEM_FOLDER] => system(conn).await,
        [_] => Ok(folders(path, &FOLDERS)),
        [db, "tables"] => tables(conn, path, db).await,
        [db, "views"] => views(conn, path, db).await,
        [db, "functions"] => routines(conn, path, db, "FUNCTION", NodeKind::Function).await,
        [db, "procedures"] => routines(conn, path, db, "PROCEDURE", NodeKind::Procedure).await,
        [db, "triggers"] => triggers(conn, path, db).await,
        [_, "tables", _] => Ok(folders(path, &["columns", "indexes"])),
        [db, "tables", t, "columns"] | [db, "views", t] => columns(conn, path, db, t).await,
        [db, "tables", t, "indexes"] => indexes(conn, path, db, t).await,
        _ => Err(unsupported(path)),
    }
}

async fn schemas(conn: &mut Conn) -> Result<Vec<String>, DriverError> {
    Ok(rows(
        conn,
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME",
        &[],
    )
    .await?
    .iter()
    .map(|r| string(r, 0))
    .collect())
}

async fn root(
    conn: &mut Conn,
    current: Option<&str>,
    show_all: bool,
) -> Result<Vec<Node>, DriverError> {
    let root = NodePath::root();
    let mut nodes: Vec<Node> = schemas(conn)
        .await?
        .into_iter()
        .filter(|s| !is_system(s))
        .filter(|s| show_all || Some(s.as_str()) == current)
        .map(|s| {
            let detail = (Some(s.as_str()) == current).then(|| "current".to_string());
            node(&root, s, NodeKind::Schema, true, detail)
        })
        .collect();
    nodes.push(Node {
        path: root.child(SYSTEM_FOLDER),
        name: "System".into(),
        kind: NodeKind::Folder,
        has_children: true,
        detail: None,
    });
    Ok(nodes)
}

/// The server's own schemas. Their paths start at the root like any database's.
async fn system(conn: &mut Conn) -> Result<Vec<Node>, DriverError> {
    let root = NodePath::root();
    Ok(schemas(conn)
        .await?
        .into_iter()
        .filter(|s| is_system(s))
        .map(|s| node(&root, s, NodeKind::Schema, true, None))
        .collect())
}

async fn tables(conn: &mut Conn, path: &NodePath, db: &str) -> Result<Vec<Node>, DriverError> {
    let rows = rows(
        conn,
        "SELECT TABLE_NAME, TABLE_ROWS FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = ? AND TABLE_TYPE NOT IN ('VIEW', 'SYSTEM VIEW') \
         ORDER BY TABLE_NAME",
        &[db],
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let detail = int(r, 1).map(|n| format!("~{n} rows"));
            node(path, string(r, 0), NodeKind::Table, true, detail)
        })
        .collect())
}

async fn views(conn: &mut Conn, path: &NodePath, db: &str) -> Result<Vec<Node>, DriverError> {
    let rows = rows(
        conn,
        "SELECT TABLE_NAME FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = ? AND TABLE_TYPE IN ('VIEW', 'SYSTEM VIEW') ORDER BY TABLE_NAME",
        &[db],
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| node(path, string(r, 0), NodeKind::View, true, None))
        .collect())
}

/// Functions or procedures (`routine_type`); a function and a procedure may share a name,
/// so each kind has its own folder.
async fn routines(
    conn: &mut Conn,
    path: &NodePath,
    db: &str,
    routine_type: &str,
    kind: NodeKind,
) -> Result<Vec<Node>, DriverError> {
    let rows = rows(
        conn,
        "SELECT ROUTINE_NAME, DTD_IDENTIFIER FROM information_schema.ROUTINES \
         WHERE ROUTINE_SCHEMA = ? AND ROUTINE_TYPE = ? ORDER BY ROUTINE_NAME",
        &[db, routine_type],
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let detail = text(r, 1).map(|ty| format!("returns {ty}"));
            node(path, string(r, 0), kind, false, detail)
        })
        .collect())
}

async fn triggers(conn: &mut Conn, path: &NodePath, db: &str) -> Result<Vec<Node>, DriverError> {
    let rows = rows(
        conn,
        "SELECT TRIGGER_NAME, ACTION_TIMING, EVENT_MANIPULATION, EVENT_OBJECT_TABLE \
         FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ? ORDER BY TRIGGER_NAME",
        &[db],
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let detail = format!("{} {} ON {}", string(r, 1), string(r, 2), string(r, 3));
            node(path, string(r, 0), NodeKind::Trigger, false, Some(detail))
        })
        .collect())
}

async fn columns(
    conn: &mut Conn,
    path: &NodePath,
    db: &str,
    table: &str,
) -> Result<Vec<Node>, DriverError> {
    let rows = rows(
        conn,
        "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY \
         FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
         ORDER BY ORDINAL_POSITION",
        &[db, table],
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let mut detail = string(r, 1);
            if string(r, 3) == "PRI" {
                detail.push_str(" PK");
            }
            if string(r, 2) == "NO" {
                detail.push_str(" NOT NULL");
            }
            node(path, string(r, 0), NodeKind::Column, false, Some(detail))
        })
        .collect())
}

async fn indexes(
    conn: &mut Conn,
    path: &NodePath,
    db: &str,
    table: &str,
) -> Result<Vec<Node>, DriverError> {
    let rows = rows(
        conn,
        "SELECT INDEX_NAME, MIN(NON_UNIQUE), \
                GROUP_CONCAT(COLUMN_NAME ORDER BY SEQ_IN_INDEX SEPARATOR ', ') \
         FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
         GROUP BY INDEX_NAME ORDER BY INDEX_NAME",
        &[db, table],
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let columns = text(r, 2).unwrap_or_else(|| "expression".into());
            let unique = if int(r, 1) == Some(0) { "UNIQUE " } else { "" };
            let detail = format!("{unique}({columns})");
            node(path, string(r, 0), NodeKind::Index, false, Some(detail))
        })
        .collect())
}
