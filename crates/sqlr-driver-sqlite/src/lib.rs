//! SQLite driver for sqlr: one worker thread per connection owns the `rusqlite::Connection`
//! (spec §5.6). Values keep SQLite's dynamic typing: each cell is decoded by its storage class.

mod admin;
mod browse;
mod catalog;
mod ddl_gen;
mod dialect;
mod errors;
mod execute;
mod introspect;
mod manifest;
mod rebuild;
mod snapshot;
mod syntax;
mod types;
mod worker;

use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::OpenFlags;
use sqlr_core::{
    CancelHandle, Connection, ConnectionConfig, Driver, DriverError, DriverManifest, ErrorKind,
    Execution, Introspect, Node, NodeKind, NodePath, ObjectDef, ObjectKind, ObjectRef,
    RelationalConn, SchemaCatalog, Secrets, ServerInfo, TableInfo, TableRef, Value,
};

pub use dialect::SqliteDialect;
pub use syntax::{SQLITE_KEYWORDS, sqlite_syntax};

pub(crate) use errors::map_error;
use worker::Worker;

pub struct SqliteDriver {
    manifest: DriverManifest,
}

impl SqliteDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            manifest: manifest::manifest(),
        })
    }
}

#[async_trait::async_trait]
impl Driver for SqliteDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    fn dialect(&self) -> Option<Arc<dyn sqlr_core::Dialect>> {
        Some(Arc::new(SqliteDialect))
    }

    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        _secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        sqlr_core::validate_driver_properties(&self.manifest.fields, &cfg.values)?;
        let m = &self.manifest;
        let path = cfg
            .value(m, "path")
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| DriverError::new(ErrorKind::Other, "choose a database file"))?;
        let flag = |key: &str| cfg.value(m, key).is_some_and(|v| v == "true");
        let flags = if flag("read_only") {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else if flag("create_if_missing") {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let worker = Worker::open(PathBuf::from(path), flags).await?;
        let properties: Vec<_> = cfg
            .values
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix("property.")
                    .map(|k| (k.to_string(), v.clone()))
            })
            .collect();
        worker
            .call(move |conn| -> Result<(), DriverError> {
                for (key, value) in properties {
                    conn.pragma_update(
                        None,
                        &key,
                        if value == "true" {
                            "ON"
                        } else if value == "false" {
                            "OFF"
                        } else {
                            &value
                        },
                    )
                    .map_err(|e| {
                        DriverError::new(ErrorKind::Other, format!("Driver property {key}: {e}"))
                    })?;
                }
                Ok(())
            })
            .await??;

        Ok(Arc::new(SqliteConnection {
            worker: Arc::new(worker),
            dialect: SqliteDialect,
        }))
    }
}

pub struct SqliteConnection {
    worker: Arc<Worker>,
    dialect: SqliteDialect,
}

#[async_trait::async_trait]
impl Connection for SqliteConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let version = self
            .worker
            .call(|conn| {
                conn.query_row("SELECT sqlite_version()", [], |r| r.get::<_, String>(0))
                    .map_err(map_error)
            })
            .await??;
        Ok(ServerInfo {
            product: "SQLite".into(),
            version,
        })
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        let path = path.clone();
        self.worker
            .call(move |conn| browse::browse(conn, &path))
            .await?
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(SqliteCancel {
            worker: self.worker.clone(),
        })
    }

    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl RelationalConn for SqliteConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        execute::execute(&self.worker, sql.to_string(), Vec::new()).await
    }

    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<Execution, DriverError> {
        execute::execute(&self.worker, sql.to_string(), params.to_vec()).await
    }

    async fn describe(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        let table = table.clone();
        self.worker
            .call(move |conn| catalog::describe(conn, &table))
            .await?
    }

    /// The statements SQLite keeps for the object, as they were written: they are its
    /// definition, comments and formatting included.
    async fn ddl(&self, node: &Node) -> Result<String, DriverError> {
        let node = node.clone();
        self.worker
            .call(move |conn| catalog::ddl(conn, &node))
            .await?
    }

    /// Paths are `[schema, folder, name]`.
    fn object_ref(&self, node: &Node) -> Option<ObjectRef> {
        let [schema, _folder, name] = node.path.segments() else {
            return None;
        };
        let kind = match node.kind {
            NodeKind::Table => ObjectKind::Table,
            NodeKind::View => ObjectKind::View,
            NodeKind::Index => ObjectKind::Index,
            NodeKind::Trigger => ObjectKind::Trigger,
            _ => return None,
        };
        Some(ObjectRef {
            kind,
            database: None,
            schema: Some(schema.clone()),
            name: name.clone(),
            parent: None,
            signature: None,
        })
    }

    fn introspect(&self) -> Option<&dyn Introspect> {
        Some(self)
    }

    fn admin(&self) -> Option<&dyn sqlr_core::Admin> {
        Some(self)
    }

    fn dialect(&self) -> &dyn sqlr_core::Dialect {
        &self.dialect
    }

    async fn catalog(&self, schema: &str) -> Result<SchemaCatalog, DriverError> {
        let schema = schema.to_string();
        self.worker
            .call(move |conn| catalog::catalog(conn, &schema))
            .await?
    }
}

#[async_trait::async_trait]
impl Introspect for SqliteConnection {
    async fn load_object(&self, object: &ObjectRef) -> Result<ObjectDef, DriverError> {
        let object = object.clone();
        self.worker
            .call(move |conn| introspect::load_object(conn, &object))
            .await?
    }

    /// SQLite has no users or roles.
    async fn list_roles(&self) -> Result<Vec<String>, DriverError> {
        Ok(Vec::new())
    }

    async fn load_schema(
        &self,
        _database: Option<&str>,
        schema: &str,
    ) -> Result<sqlr_core::SchemaSnapshot, DriverError> {
        let schema = schema.to_string();
        self.worker
            .call(move |conn| snapshot::load_schema(conn, &schema))
            .await?
    }
}

struct SqliteCancel {
    worker: Arc<Worker>,
}

#[async_trait::async_trait]
impl CancelHandle for SqliteCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        self.worker.interrupt();
        Ok(())
    }
}
