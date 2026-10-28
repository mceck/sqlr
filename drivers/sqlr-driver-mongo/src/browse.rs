//! The explorer tree: databases → Collections / Views / System → collections → indexes.

use sqlr_core::{DocValue, DriverError, Node, NodeKind, NodePath};

use crate::connect::MongoConnection;
use crate::query::{command, cursor_docs};

/// Folder path segments under a database, with their labels.
const FOLDERS: [(&str, &str); 3] = [
    ("collections", "Collections"),
    ("views", "Views"),
    ("system", "System"),
];

fn text(v: Option<&DocValue>) -> Option<&str> {
    match v {
        Some(DocValue::String(s)) => Some(s),
        _ => None,
    }
}

fn number(v: Option<&DocValue>) -> Option<i64> {
    match v {
        Some(DocValue::Int32(i)) => Some(i64::from(*i)),
        Some(DocValue::Int64(i)) => Some(*i),
        Some(DocValue::Double(d)) => Some(*d as i64),
        _ => None,
    }
}

/// `1.5 MB`-style sizes.
pub(crate) fn human_bytes(n: i64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < units.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", units[u])
    }
}

/// `(name, type, options)` of every collection, view and system collection of `db`.
async fn collections(
    conn: &MongoConnection,
    db: &str,
) -> Result<Vec<(String, String, DocValue)>, DriverError> {
    let cmd = DocValue::Document(vec![
        ("listCollections".into(), DocValue::Int32(1)),
        ("authorizedCollections".into(), DocValue::Bool(true)),
    ]);
    let mut out: Vec<(String, String, DocValue)> = cursor_docs(conn, db, &cmd)
        .await?
        .into_iter()
        .filter_map(|d| {
            let name = text(d.get("name"))?.to_string();
            let ty = text(d.get("type")).unwrap_or("collection").to_string();
            let options = d.get("options").cloned().unwrap_or(DocValue::Null);
            Some((name, ty, options))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn folder_of(name: &str, ty: &str) -> &'static str {
    if name.starts_with("system.") {
        "system"
    } else if ty == "view" {
        "views"
    } else {
        "collections"
    }
}

pub(crate) async fn browse(
    conn: &MongoConnection,
    path: &NodePath,
) -> Result<Vec<Node>, DriverError> {
    match path.segments() {
        [] => {
            let cmd = DocValue::Document(vec![("listDatabases".into(), DocValue::Int32(1))]);
            let reply = command(conn, "admin", &cmd).await?;
            let mut nodes: Vec<Node> = match reply.get("databases") {
                Some(DocValue::Array(dbs)) => dbs
                    .iter()
                    .filter_map(|d| {
                        let name = text(d.get("name"))?;
                        Some(Node {
                            path: path.child(name),
                            name: name.to_string(),
                            kind: NodeKind::Database,
                            has_children: true,
                            detail: number(d.get("sizeOnDisk")).map(human_bytes),
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            };
            nodes.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(nodes)
        }
        [db] => {
            let all = collections(conn, db).await?;
            Ok(FOLDERS
                .iter()
                .filter(|(seg, _)| {
                    *seg != "system" || all.iter().any(|(n, t, _)| folder_of(n, t) == "system")
                })
                .map(|(seg, label)| Node {
                    path: path.child(seg),
                    name: label.to_string(),
                    kind: NodeKind::Folder,
                    has_children: true,
                    detail: None,
                })
                .collect())
        }
        [db, folder] => {
            let all = collections(conn, db).await?;
            Ok(all
                .into_iter()
                .filter(|(n, t, _)| folder_of(n, t) == folder.as_str())
                .map(|(name, ty, options)| {
                    let view = ty == "view";
                    let detail = match ty.as_str() {
                        "view" => text(options.get("viewOn")).map(|on| format!("on {on}")),
                        "timeseries" => Some("time series".into()),
                        _ => None,
                    };
                    Node {
                        path: path.child(&name),
                        name,
                        kind: if view {
                            NodeKind::View
                        } else {
                            NodeKind::Table
                        },
                        has_children: !view,
                        detail,
                    }
                })
                .collect())
        }
        [db, _, coll] => {
            let cmd =
                DocValue::Document(vec![("listIndexes".into(), DocValue::String(coll.clone()))]);
            Ok(cursor_docs(conn, db, &cmd)
                .await?
                .into_iter()
                .filter_map(|ix| {
                    let name = text(ix.get("name"))?.to_string();
                    let mut detail = ix.get("key").map_or(String::new(), |k| k.to_relaxed_json());
                    if ix.get("unique") == Some(&DocValue::Bool(true)) {
                        detail.push_str(" unique");
                    }
                    Some(Node {
                        path: path.child(&name),
                        name,
                        kind: NodeKind::Index,
                        has_children: false,
                        detail: Some(detail),
                    })
                })
                .collect())
        }
        _ => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(73_728), "72.0 KB");
    }
}
