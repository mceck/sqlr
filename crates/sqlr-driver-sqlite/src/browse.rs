//! Explorer tree: attached databases → object folders → objects → columns.
//! Values are bound as parameters; schema names (which cannot be) go through `quote_ident`.

use rusqlite::Connection;
use sqlr_core::{Dialect, DriverError, ErrorKind, Node, NodeKind, NodePath};

use crate::dialect::SqliteDialect;
use crate::map_error;

pub(crate) const FOLDERS: [(&str, &str, NodeKind); 4] = [
    ("tables", "table", NodeKind::Table),
    ("views", "view", NodeKind::View),
    ("indexes", "index", NodeKind::Index),
    ("triggers", "trigger", NodeKind::Trigger),
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

pub(crate) fn browse(conn: &Connection, path: &NodePath) -> Result<Vec<Node>, DriverError> {
    let segs: Vec<&str> = path.segments().iter().map(String::as_str).collect();
    match segs.as_slice() {
        [] => schemas(conn, path),
        [_] => Ok(FOLDERS
            .iter()
            .map(|(f, _, _)| node(path, f.to_string(), NodeKind::Folder, true, None))
            .collect()),
        [schema, folder] => match FOLDERS.iter().find(|(f, _, _)| f == folder) {
            Some((_, ty, kind)) => objects(conn, path, schema, ty, *kind),
            None => Err(unsupported(path)),
        },
        [schema, "tables" | "views", relation] => columns(conn, path, schema, relation),
        _ => Err(unsupported(path)),
    }
}

fn unsupported(path: &NodePath) -> DriverError {
    DriverError::new(
        ErrorKind::Unsupported,
        format!("cannot browse {:?}", path.segments()),
    )
}

/// `main`, `temp` when it holds objects, then attached databases.
fn schemas(conn: &Connection, path: &NodePath) -> Result<Vec<Node>, DriverError> {
    let mut stmt = conn
        .prepare("SELECT name, file FROM pragma_database_list ORDER BY seq")
        .map_err(map_error)?;
    let list: Vec<(String, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })
        .map_err(map_error)?
        .collect::<Result<_, _>>()
        .map_err(map_error)?;
    let mut nodes = Vec::new();
    for (name, file) in list {
        if name == "temp" {
            let used: i64 = conn
                .query_row("SELECT count(*) FROM temp.sqlite_schema", [], |r| r.get(0))
                .map_err(map_error)?;
            if used == 0 {
                continue;
            }
        }
        let detail = (!file.is_empty()).then_some(file);
        nodes.push(node(path, name, NodeKind::Schema, true, detail));
    }
    Ok(nodes)
}

fn objects(
    conn: &Connection,
    path: &NodePath,
    schema: &str,
    ty: &str,
    kind: NodeKind,
) -> Result<Vec<Node>, DriverError> {
    let sql = format!(
        "SELECT name, tbl_name FROM {}.sqlite_schema \
         WHERE type = ?1 AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
           AND name NOT IN (SELECT name FROM pragma_table_list \
                            WHERE schema = ?2 AND type = 'shadow') \
         ORDER BY name",
        SqliteDialect.quote_ident(schema)
    );
    let mut stmt = conn.prepare(&sql).map_err(map_error)?;
    let rows: Vec<(String, String)> = stmt
        .query_map([ty, schema], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(map_error)?
        .collect::<Result<_, _>>()
        .map_err(map_error)?;
    let relation = matches!(kind, NodeKind::Table | NodeKind::View);
    Ok(rows
        .into_iter()
        .map(|(name, table)| {
            let detail = (!relation).then(|| format!("on {table}"));
            node(path, name, kind, relation, detail)
        })
        .collect())
}

fn columns(
    conn: &Connection,
    path: &NodePath,
    schema: &str,
    relation: &str,
) -> Result<Vec<Node>, DriverError> {
    let mut stmt = conn
        .prepare(
            "SELECT name, type, \"notnull\", pk FROM pragma_table_xinfo(?1, ?2) \
             WHERE hidden <> 1 ORDER BY cid",
        )
        .map_err(map_error)?;
    let rows: Vec<(String, String, bool, i64)> = stmt
        .query_map([relation, schema], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(map_error)?
        .collect::<Result<_, _>>()
        .map_err(map_error)?;
    Ok(rows
        .into_iter()
        .map(|(name, ty, not_null, pk)| {
            let mut detail = vec![ty];
            if pk > 0 {
                detail.push("PK".into());
            } else if not_null {
                detail.push("NOT NULL".into());
            }
            let detail = detail
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            node(path, name, NodeKind::Column, false, Some(detail))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_table_shadow_tables_are_not_listed_as_tables() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE VIRTUAL TABLE docs USING fts5(body)")
            .unwrap();
        let nodes = browse(&conn, &NodePath(vec!["main".into(), "tables".into()])).unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"docs"), "{names:?}");
        assert!(
            !names.iter().any(|n| n.starts_with("docs_")),
            "shadow tables leaked into the explorer: {names:?}"
        );
    }
}
