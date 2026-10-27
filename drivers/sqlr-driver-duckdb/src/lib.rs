//! DuckDB sidecar driver: the bundled engine, with one worker thread per connection.
//! Sessions of one connection share a native database inside the sidecar process.

#![cfg_attr(not(test), warn(clippy::unwrap_used))]

mod catalog;
mod dialect;
mod execute;
mod manifest;
mod types;
mod worker;

pub use dialect::DuckdbDialect;
use sqlr_driver_sdk::{
    CancelHandle, Connection, ConnectionConfig, Driver, DriverError, DriverManifest, ErrorKind,
    Execution, Node, NodePath, RelationalConn, SchemaCatalog, Secrets, ServerInfo, TableInfo,
    TableRef, Value,
};
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};
use worker::Worker;

pub struct DuckdbDriver {
    manifest: DriverManifest,
    databases: tokio::sync::Mutex<HashMap<String, Weak<Database>>>,
}
impl DuckdbDriver {
    pub fn new() -> Self {
        Self {
            manifest: manifest::manifest(),
            databases: Default::default(),
        }
    }
}
impl Default for DuckdbDriver {
    fn default() -> Self {
        Self::new()
    }
}
#[async_trait::async_trait]
impl Driver for DuckdbDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }
    fn dialect(&self) -> Option<Arc<dyn sqlr_driver_sdk::Dialect>> {
        Some(Arc::new(DuckdbDialect))
    }
    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        _: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        let path = cfg
            .value(&self.manifest, "path")
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(|| {
                DriverError::new(
                    ErrorKind::Connection,
                    "choose a DuckDB database file or :memory:",
                )
            })?;
        let read_only = cfg.value(&self.manifest, "read_only") == Some("true");
        let key = if path == ":memory:" {
            format!("memory:{}", cfg.id)
        } else {
            format!("file:{path}")
        };
        let key = format!("{key}:read_only={read_only}");
        let mut databases = self.databases.lock().await;
        let database = match databases.get(&key).and_then(Weak::upgrade) {
            Some(database) => database,
            None => {
                let database = Arc::new(Database {
                    anchor: Worker::open(path.into(), read_only).await?,
                });
                databases.insert(key, Arc::downgrade(&database));
                database
            }
        };
        let worker = Arc::new(database.anchor.clone_connection().await?);
        Ok(Arc::new(DuckdbConnection {
            worker,
            dialect: DuckdbDialect,
            _database: database,
        }))
    }
}
/// A weak cache entry keeps one native database alive while any of its sessions exists.
struct Database {
    anchor: Worker,
}

struct DuckdbConnection {
    _database: Arc<Database>,
    worker: Arc<Worker>,
    dialect: DuckdbDialect,
}
#[async_trait::async_trait]
impl Connection for DuckdbConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let version = self
            .worker
            .call(|conn| {
                conn.query_row("SELECT version()", [], |row| row.get::<_, String>(0))
                    .map_err(map_error)
            })
            .await??;
        Ok(ServerInfo {
            product: "DuckDB".into(),
            version,
        })
    }
    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        let path = path.clone();
        self.worker
            .call(move |conn| catalog::browse(conn, &path))
            .await?
    }
    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(DuckdbCancel(self.worker.clone()))
    }
    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }
}
#[async_trait::async_trait]
impl RelationalConn for DuckdbConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        execute::execute(&self.worker, sql.into(), Vec::new()).await
    }
    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<Execution, DriverError> {
        execute::execute(&self.worker, sql.into(), params.to_vec()).await
    }
    async fn describe(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        let table = table.clone();
        self.worker
            .call(move |conn| catalog::describe(conn, &table))
            .await?
    }
    async fn catalog(&self, schema: &str) -> Result<SchemaCatalog, DriverError> {
        let schema = schema.to_string();
        self.worker
            .call(move |conn| catalog::catalog(conn, &schema))
            .await?
    }
    fn dialect(&self) -> &dyn sqlr_driver_sdk::Dialect {
        &self.dialect
    }
}
struct DuckdbCancel(Arc<Worker>);
#[async_trait::async_trait]
impl CancelHandle for DuckdbCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        self.0.interrupt();
        Ok(())
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` function pointer, which receives the error by value"
)]
pub(crate) fn map_error(error: duckdb::Error) -> DriverError {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    let kind = if lower.contains("interrupt") {
        ErrorKind::Cancelled
    } else if lower.contains("parser error") || lower.contains("syntax error") {
        ErrorKind::Syntax
    } else if lower.contains("read-only")
        || lower.contains("read only")
        || lower.contains("permission")
    {
        ErrorKind::Permission
    } else {
        ErrorKind::Other
    };
    DriverError::new(kind, message)
}
