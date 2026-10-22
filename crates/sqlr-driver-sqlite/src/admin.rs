//! Administration for SQLite: the database's properties (PRAGMAs, the library version and
//! an integrity check) and statistics (database file sizes; per table, an exact row count
//! and, through the `dbstat` virtual table compiled into the bundled SQLite, its bytes).
//! Row counts come from `sqlite_stat1` when `ANALYZE` ran, else from a `count(*)` that a
//! cancel stops. SQLite has no users, grants or sessions. Pragma names are constants;
//! schema and table names are quoted through the dialect, and the `dbstat` schema is a
//! bound parameter.

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::Connection;
use rusqlite::types::Value;
use sqlr_core::{
    Admin, AdminSupport, CancelHandle, Dialect, DriverError, ErrorKind, ServerProperty, TableRef,
    TableStats,
};

use crate::dialect::SqliteDialect;
use crate::worker::{AdminStop, Worker};
use crate::{SqliteConnection, map_error};

/// The pragmas shown, with their category and a description.
const PRAGMAS: [(&str, &str, &str); 12] = [
    ("page_size", "Storage", "Bytes per database page"),
    ("page_count", "Storage", "Pages in the database file"),
    (
        "freelist_count",
        "Storage",
        "Unused pages in the database file",
    ),
    ("auto_vacuum", "Storage", "0 none, 1 full, 2 incremental"),
    ("journal_mode", "Journal", "How transactions are journaled"),
    ("synchronous", "Journal", "0 off, 1 normal, 2 full, 3 extra"),
    ("encoding", "Database", "Text encoding of the database"),
    (
        "user_version",
        "Database",
        "Version number kept by the application",
    ),
    (
        "application_id",
        "Database",
        "Application id in the file header",
    ),
    (
        "schema_version",
        "Database",
        "Changes whenever the schema changes",
    ),
    (
        "foreign_keys",
        "Behavior",
        "Foreign keys are enforced on this connection",
    ),
    ("cache_size", "Memory", "Page cache size (negative: KiB)"),
];

fn text(value: Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Integer(n) => n.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Text(s) => s,
        Value::Blob(b) => format!("{} bytes", b.len()),
    }
}

fn property(name: &str, value: String, category: &str, description: &str) -> ServerProperty {
    ServerProperty {
        name: name.to_string(),
        value,
        unit: None,
        category: Some(category.to_string()),
        description: Some(description.to_string()),
        source: None,
    }
}

fn properties(conn: &Connection) -> Result<Vec<ServerProperty>, DriverError> {
    let mut out = Vec::new();
    let version: String = conn
        .query_row("SELECT sqlite_version()", [], |r| r.get(0))
        .map_err(map_error)?;
    out.push(property(
        "sqlite_version",
        version,
        "Database",
        "Version of the SQLite library",
    ));
    for (name, category, description) in PRAGMAS {
        let value: Value = conn
            .query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
            .map_err(map_error)?;
        out.push(property(name, text(value), category, description));
    }
    // At most ten problems are listed.
    let mut statement = conn
        .prepare("PRAGMA integrity_check(10)")
        .map_err(map_error)?;
    let problems: Vec<String> = statement
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(map_error)?
        .collect::<Result<_, _>>()
        .map_err(map_error)?;
    out.push(property(
        "integrity_check",
        problems.join("\n"),
        "Integrity",
        "\"ok\", or the first problems found",
    ));
    Ok(out)
}

/// The attached databases' names.
fn schemas(conn: &Connection) -> Result<Vec<String>, DriverError> {
    let mut statement = conn
        .prepare("SELECT name FROM pragma_database_list ORDER BY seq")
        .map_err(map_error)?;
    statement
        .query_map([], |r| r.get(0))
        .map_err(map_error)?
        .collect::<Result<_, _>>()
        .map_err(map_error)
}

fn database_sizes(conn: &Connection) -> Result<Vec<(String, i64)>, DriverError> {
    let d = SqliteDialect;
    let mut out = Vec::new();
    for schema in schemas(conn)? {
        let q = d.quote_ident(&schema);
        let pragma = |name: &str| -> Result<i64, DriverError> {
            conn.query_row(&format!("PRAGMA {q}.{name}"), [], |r| r.get(0))
                .map_err(map_error)
        };
        out.push((schema.clone(), pragma("page_count")? * pragma("page_size")?));
    }
    Ok(out)
}

/// Bytes per table and index of `schema` from `dbstat`; `None` when SQLite was built
/// without it.
fn dbstat(conn: &Connection, schema: &str) -> Option<HashMap<String, i64>> {
    let mut statement = conn
        .prepare("SELECT name, sum(pgsize) FROM dbstat(?1) GROUP BY name")
        .ok()?;
    let rows = statement
        .query_map([schema], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })
        .ok()?;
    rows.collect::<Result<_, _>>().ok()
}

fn cancelled() -> DriverError {
    DriverError::new(ErrorKind::Cancelled, "The statistics read was cancelled")
}

/// Row estimates of `schema`'s tables from `sqlite_stat1` (written by `ANALYZE`), by table;
/// empty when the schema was never analyzed.
fn analyzed_rows(conn: &Connection, q: &str) -> HashMap<String, i64> {
    let sql = format!("SELECT tbl, stat FROM {q}.sqlite_stat1");
    let Ok(mut statement) = conn.prepare(&sql) else {
        return HashMap::new();
    };
    let rows = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)));
    let mut out = HashMap::new();
    for (table, stat) in rows.into_iter().flatten().flatten() {
        // The first number of `stat` is the table's row count.
        if let Some(n) = stat.split_whitespace().next().and_then(|n| n.parse().ok()) {
            out.entry(table).or_insert(n);
        }
    }
    out
}

/// Table statistics; row counts come from `ANALYZE` when it ran, else from `count(*)`,
/// which `stop` interrupts (closing the tab cancels a long count).
fn table_stats(
    conn: &Connection,
    schema: Option<&str>,
    stop: &AdminStop,
    generation: u64,
) -> Result<Vec<TableStats>, DriverError> {
    stop.set_running(generation, true);
    let outcome = table_stats_stoppable(conn, schema, stop, generation);
    stop.set_running(generation, false);
    let was_stopped = stop.is_stopped(generation);
    match outcome {
        Err(e) if was_stopped || e.kind == ErrorKind::Cancelled => Err(cancelled()),
        _ if was_stopped => Err(cancelled()),
        other => other,
    }
}

fn table_stats_stoppable(
    conn: &Connection,
    schema: Option<&str>,
    stop: &AdminStop,
    generation: u64,
) -> Result<Vec<TableStats>, DriverError> {
    let d = SqliteDialect;
    let wanted = match schema {
        Some(s) => vec![s.to_string()],
        None => schemas(conn)?,
    };
    let mut out = Vec::new();
    for schema in wanted {
        let q = d.quote_ident(&schema);
        // `(type, name, table)` of the schema's tables and indexes.
        let mut statement = conn
            .prepare(&format!(
                "SELECT type, name, tbl_name FROM {q}.sqlite_schema \
                 WHERE type IN ('table', 'index') ORDER BY name"
            ))
            .map_err(map_error)?;
        let objects: Vec<(String, String, String)> = statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(map_error)?
            .collect::<Result<_, _>>()
            .map_err(map_error)?;
        let sizes = dbstat(conn, &schema);
        let analyzed = analyzed_rows(conn, &q);
        let tables = objects
            .iter()
            .filter(|(k, n, _)| k == "table" && !n.starts_with("sqlite_"));
        for (_, name, _) in tables {
            if stop.is_stopped(generation) {
                return Err(cancelled());
            }
            let rows: i64 = match analyzed.get(name) {
                Some(n) => *n,
                None => conn
                    .query_row(
                        &format!("SELECT count(*) FROM {q}.{}", d.quote_ident(name)),
                        [],
                        |r| r.get(0),
                    )
                    .map_err(map_error)?,
            };
            let (table_bytes, index_bytes) = match &sizes {
                Some(sizes) => {
                    let table = sizes.get(name).copied().unwrap_or(0);
                    let index = objects
                        .iter()
                        .filter(|(k, _, t)| k == "index" && t == name)
                        .map(|(_, n, _)| sizes.get(n).copied().unwrap_or(0))
                        .sum::<i64>();
                    (Some(table), Some(index))
                }
                None => (None, None),
            };
            out.push(TableStats {
                table: TableRef {
                    database: None,
                    schema: Some(schema.clone()),
                    name: name.clone(),
                },
                rows_estimate: Some(rows),
                total_bytes: table_bytes.zip(index_bytes).map(|(t, i)| t + i),
                table_bytes,
                index_bytes,
                extra: Vec::new(),
            });
        }
    }
    Ok(out)
}

#[async_trait::async_trait]
impl Admin for SqliteConnection {
    fn supports(&self) -> AdminSupport {
        AdminSupport {
            properties: true,
            statistics: true,
            ..Default::default()
        }
    }

    async fn properties(&self) -> Result<Vec<ServerProperty>, DriverError> {
        self.worker.call(properties).await?
    }

    async fn database_sizes(&self) -> Result<Vec<(String, i64)>, DriverError> {
        self.worker.call(database_sizes).await?
    }

    async fn table_stats(&self, schema: Option<&str>) -> Result<Vec<TableStats>, DriverError> {
        let schema = schema.map(String::from);
        let stop = self.worker.admin.clone();
        let generation = stop.claim();
        self.worker
            .call(move |conn| table_stats(conn, schema.as_deref(), &stop, generation))
            .await?
    }

    fn snapshot_cancel(&self) -> Option<Arc<dyn CancelHandle>> {
        Some(Arc::new(StopStats {
            generation: self.worker.admin.issue(),
            worker: self.worker.clone(),
        }))
    }
}

/// Stops the statistics read this handle was issued for (the next one the connection
/// queues), on the connection's thread.
struct StopStats {
    worker: Arc<Worker>,
    generation: u64,
}

#[async_trait::async_trait]
impl CancelHandle for StopStats {
    async fn cancel(&self) -> Result<(), DriverError> {
        self.worker.stop_admin(self.generation);
        Ok(())
    }
}
