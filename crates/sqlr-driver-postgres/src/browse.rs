//! Explorer tree backed by the PostgreSQL catalogs. All catalog queries are parameterized.

use sqlr_core::{DriverError, ErrorKind, Node, NodeKind, NodePath};
use tokio_postgres::Client;

use crate::map_error;

const FOLDERS: [&str; 5] = [
    "tables",
    "views",
    "materialized views",
    "functions",
    "sequences",
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

/// The connectable databases on the server, current database first.
pub(crate) async fn databases(
    client: &Client,
    path: &NodePath,
    current: &str,
) -> Result<Vec<Node>, DriverError> {
    let rows = client
        .query(
            "SELECT datname FROM pg_database WHERE datallowconn \
             ORDER BY CASE datname WHEN $1 THEN 0 ELSE 1 END, datname",
            &[&current],
        )
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            let name: String = r.get(0);
            let detail = (name == current).then(|| "current".to_string());
            node(path, name, NodeKind::Database, true, detail)
        })
        .collect())
}

pub(crate) async fn browse(client: &Client, path: &NodePath) -> Result<Vec<Node>, DriverError> {
    let segs: Vec<&str> = path.segments().iter().map(String::as_str).collect();
    match segs.as_slice() {
        [] => schemas(client, path).await,
        [_] => Ok(FOLDERS
            .iter()
            .map(|f| node(path, f.to_string(), NodeKind::Folder, true, None))
            .collect()),
        [s, "tables"] => relations(client, path, s, &["r", "p"], NodeKind::Table).await,
        [s, "views"] => relations(client, path, s, &["v"], NodeKind::View).await,
        [s, "materialized views"] => {
            relations(client, path, s, &["m"], NodeKind::MaterializedView).await
        }
        [s, "sequences"] => sequences(client, path, s).await,
        [s, "functions"] => functions(client, path, s).await,
        [_, "tables", _] => Ok(["columns", "indexes"]
            .iter()
            .map(|f| node(path, f.to_string(), NodeKind::Folder, true, None))
            .collect()),
        [s, "tables", t, "columns"] => columns(client, path, s, t, &["r", "p"]).await,
        [s, "views", v] => columns(client, path, s, v, &["v"]).await,
        [s, "materialized views", v] => columns(client, path, s, v, &["m"]).await,
        [s, "tables", t, "indexes"] => indexes(client, path, s, t).await,
        _ => Err(unsupported(path)),
    }
}

async fn schemas(client: &Client, path: &NodePath) -> Result<Vec<Node>, DriverError> {
    let rows = client
        .query(
            "SELECT nspname FROM pg_namespace \
             WHERE nspname NOT LIKE 'pg\\_toast%' AND nspname NOT LIKE 'pg\\_temp\\_%' \
             ORDER BY CASE nspname WHEN 'information_schema' THEN 1 WHEN 'pg_catalog' THEN 2 ELSE 0 END, nspname",
            &[],
        )
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| node(path, r.get(0), NodeKind::Schema, true, None))
        .collect())
}

async fn relations(
    client: &Client,
    path: &NodePath,
    schema: &str,
    relkinds: &[&str],
    kind: NodeKind,
) -> Result<Vec<Node>, DriverError> {
    let kinds: Vec<String> = relkinds.iter().map(|k| k.to_string()).collect();
    let rows = client
        .query(
            "SELECT c.relname, c.reltuples::float8 FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind::text = ANY($2) \
             ORDER BY c.relname",
            &[&schema, &kinds],
        )
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            let tuples: f64 = r.get(1);
            let detail = (kind == NodeKind::Table && tuples >= 0.0)
                .then(|| format!("~{} rows", tuples.round() as i64));
            node(path, r.get(0), kind, true, detail)
        })
        .collect())
}

async fn sequences(
    client: &Client,
    path: &NodePath,
    schema: &str,
) -> Result<Vec<Node>, DriverError> {
    let rows = client
        .query(
            "SELECT c.relname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind = 'S' ORDER BY c.relname",
            &[&schema],
        )
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| node(path, r.get(0), NodeKind::Sequence, false, None))
        .collect())
}

async fn functions(
    client: &Client,
    path: &NodePath,
    schema: &str,
) -> Result<Vec<Node>, DriverError> {
    let rows = client
        .query(
            "SELECT p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')' AS name, \
                    p.prokind = 'p' \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = $1 ORDER BY name",
            &[&schema],
        )
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            let kind = if r.get(1) {
                NodeKind::Procedure
            } else {
                NodeKind::Function
            };
            node(path, r.get(0), kind, false, None)
        })
        .collect())
}

async fn columns(
    client: &Client,
    path: &NodePath,
    schema: &str,
    relation: &str,
    relkinds: &[&str],
) -> Result<Vec<Node>, DriverError> {
    let kinds: Vec<String> = relkinds.iter().map(|k| k.to_string()).collect();
    let rows = client
        .query(
            "SELECT a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull \
             FROM pg_attribute a \
             JOIN pg_class c ON c.oid = a.attrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind::text = ANY($3) \
               AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            &[&schema, &relation, &kinds],
        )
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| {
            let ty: String = r.get(1);
            let not_null: bool = r.get(2);
            let detail = if not_null {
                format!("{ty} NOT NULL")
            } else {
                ty
            };
            node(path, r.get(0), NodeKind::Column, false, Some(detail))
        })
        .collect())
}

async fn indexes(
    client: &Client,
    path: &NodePath,
    schema: &str,
    table: &str,
) -> Result<Vec<Node>, DriverError> {
    let rows = client
        .query(
            "SELECT ic.relname, pg_get_indexdef(ic.oid) \
             FROM pg_index i \
             JOIN pg_class c ON c.oid = i.indrelid \
             JOIN pg_class ic ON ic.oid = i.indexrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2 ORDER BY ic.relname",
            &[&schema, &table],
        )
        .await
        .map_err(map_error)?;
    Ok(rows
        .iter()
        .map(|r| node(path, r.get(0), NodeKind::Index, false, Some(r.get(1))))
        .collect())
}
