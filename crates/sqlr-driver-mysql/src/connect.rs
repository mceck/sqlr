//! Connection setup for the MySQL/MariaDB driver.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use mysql_async::prelude::Queryable;
use mysql_async::{ClientIdentity, Conn, Opts, OptsBuilder, SslOpts};
use sqlr_core::{
    CancelHandle, Connection, ConnectionConfig, Driver, DriverError, DriverManifest, ErrorKind,
    Execution, Introspect, Node, NodeKind, NodePath, ObjectDef, ObjectKind, ObjectRef,
    RelationalConn, SchemaCatalog, Secrets, ServerInfo, TableInfo, TableRef, Value,
};
use tokio::sync::Mutex;

use crate::browse::{self, SYSTEM_FOLDER};
use crate::dialect::MyDialect;
use crate::execute::{self, Notices, QueryConn, Statement};
use crate::{catalog, introspect, manifest, map_error, types};

/// How long opening a connection may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct MysqlDriver {
    manifest: DriverManifest,
}

impl MysqlDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            manifest: manifest::manifest(),
        })
    }
}

/// Opens `opts` within [`CONNECT_TIMEOUT`].
async fn open(opts: &Opts) -> Result<Conn, DriverError> {
    match tokio::time::timeout(CONNECT_TIMEOUT, Conn::new(opts.clone())).await {
        Ok(conn) => conn.map_err(map_error),
        Err(_) => Err(DriverError::new(
            ErrorKind::Timeout,
            format!(
                "connecting to the server timed out after {} s",
                CONNECT_TIMEOUT.as_secs()
            ),
        )),
    }
}

/// Runs `KILL QUERY` on a side connection, opened on first use and reopened when it broke.
pub(crate) struct Killer {
    opts: Opts,
    conn: Mutex<Option<Conn>>,
    /// The side connection's server id once opened (0 before).
    id: std::sync::atomic::AtomicU32,
}

impl Killer {
    /// The side connection's server id, once it was opened.
    pub(crate) fn id(&self) -> Option<u32> {
        Some(self.id.load(std::sync::atomic::Ordering::Relaxed)).filter(|id| *id != 0)
    }

    pub(crate) async fn kill_query(&self, id: u32) -> Result<(), DriverError> {
        let mut conn = self.conn.lock().await;
        let mut retried = false;
        loop {
            let c = match conn.as_mut() {
                Some(c) => c,
                None => {
                    let opened = open(&self.opts).await?;
                    self.id
                        .store(opened.id(), std::sync::atomic::Ordering::Relaxed);
                    conn.insert(opened)
                }
            };
            match c.query_drop(format!("KILL QUERY {id}")).await {
                Ok(()) => return Ok(()),
                // A side connection that broke while idle is reopened once.
                Err(e) if e.is_fatal() && !retried => {
                    *conn = None;
                    retried = true;
                }
                Err(e) => return Err(map_error(e)),
            }
        }
    }
}

/// TLS settings for `ssl_mode` (`disabled`, `preferred`, `required`, `verify_ca`,
/// `verify_identity`); `None` for plain connections.
fn ssl_opts(
    mode: &str,
    ca: Option<&str>,
    cert: Option<&str>,
    key: Option<&str>,
    server_name: Option<&str>,
) -> Result<Option<SslOpts>, DriverError> {
    let invalid = |message: String| DriverError::new(ErrorKind::Other, message);
    let mut ssl = SslOpts::default();
    match mode {
        "disabled" => return Ok(None),
        // Encryption only, like the `mysql` client: the certificate is not checked.
        "preferred" | "required" => {
            ssl = ssl
                .with_danger_accept_invalid_certs(true)
                .with_danger_skip_domain_validation(true);
        }
        "verify_ca" => ssl = ssl.with_danger_skip_domain_validation(true),
        "verify_identity" => {}
        other => return Err(invalid(format!("unknown SSL mode `{other}`"))),
    }
    // Through an SSH tunnel the address is 127.0.0.1; the certificate names the real host.
    if let Some(name) = server_name {
        ssl = ssl.with_danger_tls_hostname_override(Some(name.to_string()));
    }
    if let Some(ca) = ca {
        ssl = ssl
            .with_root_certs(vec![PathBuf::from(ca).into()])
            .with_disable_built_in_roots(true);
    }
    match (cert, key) {
        (Some(cert), Some(key)) => {
            ssl = ssl.with_client_identity(Some(ClientIdentity::new(
                PathBuf::from(cert).into(),
                PathBuf::from(key).into(),
            )));
        }
        (None, None) => {}
        _ => {
            return Err(invalid(
                "set both the client certificate and the client key, or neither".into(),
            ));
        }
    }
    // rustls needs a process-wide crypto provider; another crate may have installed it.
    let _ = rustls::crypto::ring::default_provider().install_default();
    Ok(Some(ssl))
}

#[async_trait::async_trait]
impl Driver for MysqlDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    fn dialect(&self) -> Option<Arc<dyn sqlr_core::Dialect>> {
        Some(Arc::new(MyDialect::default()))
    }

    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        sqlr_core::validate_driver_properties(&self.manifest.fields, &cfg.values)?;
        let m = &self.manifest;
        // Empty optional fields count as unset.
        let value = |key: &str| cfg.value(m, key).map(str::trim).filter(|v| !v.is_empty());
        let required = |key: &str| {
            value(key).ok_or_else(|| {
                DriverError::new(
                    ErrorKind::Other,
                    format!("missing connection field `{key}`"),
                )
            })
        };
        let port: u16 = required("port")?
            .parse()
            .map_err(|_| DriverError::new(ErrorKind::Other, "invalid port"))?;
        let database = value("database").map(String::from);
        let show_all_databases = value("show_all_databases") == Some("true");
        let mode = value("ssl_mode").unwrap_or("preferred");
        let ssl = ssl_opts(
            mode,
            value("ssl_ca"),
            value("ssl_cert"),
            value("ssl_key"),
            cfg.tls_server_name(),
        )?;

        let init: Vec<String> = cfg
            .values
            .iter()
            .filter_map(|(property, value)| {
                property.strip_prefix("property.").map(|key| {
                    // Numeric session variables reject a binary string literal; send
                    // integers as numbers and everything else as a binary literal, which
                    // keeps quotes and backslashes safe for string variables.
                    let literal = if value.parse::<i64>().is_ok() {
                        value.clone()
                    } else {
                        format!(
                            "X'{}'",
                            value
                                .as_bytes()
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<String>()
                        )
                    };
                    format!("SET SESSION `{key}` = {literal}")
                })
            })
            .collect();
        let builder = OptsBuilder::default()
            .init(init)
            .ip_or_hostname(required("host")?)
            .tcp_port(port)
            .user(Some(required("user")?))
            .pass(secrets.0.get("password").cloned())
            .db_name(database.clone())
            .prefer_socket(false)
            // Affected rows count matched rows: an UPDATE to a value the row already holds
            // still affects 1, so applied edits can tell it from a concurrent change.
            .client_found_rows(true);
        let opts: Opts = builder.clone().ssl_opts(ssl).into();
        let (opts, first) = match open(&opts).await {
            Ok(conn) => (opts, conn),
            // `preferred` falls back to a plain connection when the server has no TLS.
            Err(e) if mode == "preferred" && e.message == crate::errors::NO_TLS => {
                let plain: Opts = builder.ssl_opts(None::<SslOpts>).into();
                let conn = open(&plain).await?;
                (plain, conn)
            }
            Err(e) => return Err(e),
        };
        Ok(Arc::new(
            MyConnection::open(opts, Some(first), database, show_all_databases).await?,
        ))
    }
}

pub struct MyConnection {
    /// Explorer, catalog, describe, DDL, administration and ping.
    pub(crate) meta: Mutex<Conn>,
    /// The server's id of the explorer connection.
    pub(crate) meta_id: u32,
    /// User queries; a live result stream holds this lock.
    query: Arc<Mutex<QueryConn>>,
    pub(crate) query_id: u32,
    pub(crate) killer: Arc<Killer>,
    /// The server ids of the connections to other databases, as they open.
    other_ids: StdMutex<Vec<u32>>,
    notices: Notices,
    pub(crate) dialect: MyDialect,
    /// Reused to open connections to the other databases of the same server.
    pub(crate) opts: Opts,
    database: Option<String>,
    show_all_databases: bool,
    /// Open connections to other databases, by database name.
    databases: Mutex<HashMap<String, Arc<MyConnection>>>,
}

impl MyConnection {
    async fn open(
        opts: Opts,
        meta: Option<Conn>,
        database: Option<String>,
        show_all_databases: bool,
    ) -> Result<Self, DriverError> {
        let mut meta = match meta {
            Some(conn) => conn,
            None => open(&opts).await?,
        };
        let version: Option<String> = meta
            .query_first("SELECT VERSION()")
            .await
            .map_err(map_error)?;
        let mariadb = version.is_some_and(|v| v.to_ascii_lowercase().contains("mariadb"));
        let sql_mode: Option<String> = meta
            .query_first("SELECT @@SESSION.sql_mode")
            .await
            .map_err(map_error)?;
        let no_backslash_escapes =
            sql_mode.is_some_and(|m| m.to_ascii_uppercase().contains("NO_BACKSLASH_ESCAPES"));
        let query = open(&opts).await?;
        let query_id = query.id();
        let meta_id = meta.id();
        Ok(Self {
            meta: Mutex::new(meta),
            meta_id,
            query: Arc::new(Mutex::new(QueryConn::new(query))),
            query_id,
            killer: Arc::new(Killer {
                opts: opts.clone(),
                conn: Mutex::new(None),
                id: std::sync::atomic::AtomicU32::new(0),
            }),
            other_ids: StdMutex::new(Vec::new()),
            notices: Arc::new(StdMutex::new(Vec::new())),
            dialect: MyDialect {
                database: database.clone(),
                mariadb,
                no_backslash_escapes,
            },
            opts,
            database,
            show_all_databases,
            databases: Mutex::new(HashMap::new()),
        })
    }

    /// The server ids of this connection's own sessions: explorer, queries, the KILL
    /// side connection once opened, and the connections to other databases.
    pub(crate) fn own_ids(&self) -> Vec<u32> {
        let others = self
            .other_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        [self.meta_id, self.query_id]
            .into_iter()
            .chain(self.killer.id())
            .chain(others)
            .collect()
    }

    /// A connection to another database of the same server, opened on first use.
    async fn database_connection(&self, database: &str) -> Result<Arc<Self>, DriverError> {
        if self.database.as_deref() == Some(database) {
            return Err(DriverError::new(
                ErrorKind::Other,
                "the connection is already attached to that database",
            ));
        }
        let mut databases = self.databases.lock().await;
        if let Some(conn) = databases.get(database) {
            return Ok(conn.clone());
        }
        let opts: Opts = OptsBuilder::from_opts(self.opts.clone())
            .db_name(Some(database))
            .into();
        let conn = Arc::new(Self::open(opts, None, Some(database.to_string()), false).await?);
        self.other_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend([conn.meta_id, conn.query_id]);
        databases.insert(database.to_string(), conn.clone());
        Ok(conn)
    }

    /// The database `table` lives in: its schema, else this connection's.
    fn table_database<'a>(&'a self, table: &'a TableRef) -> Result<&'a str, DriverError> {
        table
            .schema
            .as_deref()
            .or(table.database.as_deref())
            .or(self.database.as_deref())
            .ok_or_else(|| {
                DriverError::new(
                    ErrorKind::Other,
                    format!("no database selected for table `{}`", table.name),
                )
            })
    }
}

#[async_trait::async_trait]
impl Connection for MyConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let mut meta = self.meta.lock().await;
        let version: Option<String> = meta
            .query_first("SELECT VERSION()")
            .await
            .map_err(map_error)?;
        let version = version.unwrap_or_default();
        let product = if version.to_ascii_lowercase().contains("mariadb") {
            "MariaDB"
        } else {
            "MySQL"
        };
        Ok(ServerInfo {
            product: product.into(),
            version,
        })
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        let mut meta = self.meta.lock().await;
        let current = self.database.as_deref();
        let show_all = self.show_all_databases || current.is_none();
        browse::browse(&mut meta, path, current, show_all).await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(MyCancel {
            killer: self.killer.clone(),
            id: self.query_id,
        })
    }

    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }

    fn database_name(&self) -> Option<&str> {
        self.database.as_deref()
    }

    /// Databases are listed at the root: data under another database runs on a connection
    /// to it, so unqualified names resolve there.
    fn database_for_path(&self, path: &NodePath) -> Option<String> {
        path.segments()
            .first()
            .filter(|name| name.as_str() != SYSTEM_FOLDER)
            .filter(|name| Some(name.as_str()) != self.database.as_deref())
            .cloned()
    }

    async fn connect_database(&self, database: &str) -> Result<Arc<dyn Connection>, DriverError> {
        let conn = self.database_connection(database).await?;
        Ok(conn)
    }
}

#[async_trait::async_trait]
impl RelationalConn for MyConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        execute::execute(
            self.query.clone(),
            self.killer.clone(),
            self.notices.clone(),
            Statement::Text(sql),
        )
        .await
    }

    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<Execution, DriverError> {
        execute::execute(
            self.query.clone(),
            self.killer.clone(),
            self.notices.clone(),
            Statement::Prepared(sql, params.iter().map(types::param).collect()),
        )
        .await
    }

    async fn describe(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        let database = self.table_database(table)?;
        let mut meta = self.meta.lock().await;
        catalog::describe(&mut meta, database, &table.name).await
    }

    /// Paths are `[database]`, `[database, folder, name]` and
    /// `[database, "tables", table, "indexes", index]`.
    fn object_ref(&self, node: &Node) -> Option<ObjectRef> {
        let segs: Vec<&str> = node.path.segments().iter().map(String::as_str).collect();
        let object = |kind, db: &str, name: &str, parent: Option<&str>| ObjectRef {
            kind,
            database: None,
            schema: Some(db.to_string()),
            name: name.to_string(),
            parent: parent.map(String::from),
            signature: None,
        };
        let kind = match (node.kind, segs.as_slice()) {
            (NodeKind::Schema, [db]) => {
                return Some(ObjectRef {
                    schema: None,
                    ..object(ObjectKind::Database, db, db, None)
                });
            }
            (NodeKind::Index, [db, "tables", t, "indexes", i]) => {
                return Some(object(ObjectKind::Index, db, i, Some(t)));
            }
            (NodeKind::Table, [_, "tables", _]) => ObjectKind::Table,
            (NodeKind::View, [_, "views", _]) => ObjectKind::View,
            (NodeKind::Function, [_, "functions", _]) => ObjectKind::Function,
            (NodeKind::Procedure, [_, "procedures", _]) => ObjectKind::Procedure,
            (NodeKind::Trigger, [_, "triggers", _]) => ObjectKind::Trigger,
            _ => return None,
        };
        Some(object(kind, segs[0], segs[2], None))
    }

    fn introspect(&self) -> Option<&dyn Introspect> {
        Some(self)
    }

    fn admin(&self) -> Option<&dyn sqlr_core::Admin> {
        Some(self)
    }

    async fn take_notices(&self) -> Vec<String> {
        std::mem::take(&mut *self.notices.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn dialect(&self) -> &dyn sqlr_core::Dialect {
        &self.dialect
    }

    async fn catalog(&self, schema: &str) -> Result<SchemaCatalog, DriverError> {
        let mut meta = self.meta.lock().await;
        catalog::catalog(&mut meta, schema).await
    }
}

#[async_trait::async_trait]
impl Introspect for MyConnection {
    /// Runs on the explorer connection; `information_schema` covers every database.
    async fn load_object(&self, object: &ObjectRef) -> Result<ObjectDef, DriverError> {
        let mut meta = self.meta.lock().await;
        introspect::load_object(
            &mut meta,
            object,
            self.database.as_deref(),
            self.dialect.mariadb,
        )
        .await
    }

    async fn list_roles(&self) -> Result<Vec<String>, DriverError> {
        let mut meta = self.meta.lock().await;
        introspect::users(&mut meta).await
    }

    /// A database in a fixed number of `information_schema` queries (`schema` is the
    /// database: MySQL has no schemas below it).
    async fn load_schema(
        &self,
        _database: Option<&str>,
        schema: &str,
    ) -> Result<sqlr_core::SchemaSnapshot, DriverError> {
        let mut meta = self.meta.lock().await;
        crate::snapshot::load_schema(&mut meta, schema, self.dialect.mariadb).await
    }
}

struct MyCancel {
    killer: Arc<Killer>,
    id: u32,
}

#[async_trait::async_trait]
impl CancelHandle for MyCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        self.killer.kill_query(self.id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssl_modes() {
        assert!(
            ssl_opts("disabled", None, None, None, None)
                .unwrap()
                .is_none()
        );
        let required = ssl_opts("required", None, None, None, None)
            .unwrap()
            .unwrap();
        assert!(required.accept_invalid_certs());
        let verify = ssl_opts("verify_ca", Some("/ca.pem"), None, None, None)
            .unwrap()
            .unwrap();
        assert!(!verify.accept_invalid_certs() && verify.skip_domain_validation());
        assert!(verify.disable_built_in_roots());
        let full = ssl_opts("verify_identity", None, None, None, None)
            .unwrap()
            .unwrap();
        assert!(!full.accept_invalid_certs() && !full.skip_domain_validation());
        assert!(ssl_opts("bogus", None, None, None, None).is_err());
        assert!(ssl_opts("required", None, Some("/c.pem"), None, None).is_err());
        let with_identity = ssl_opts("required", None, Some("/c.pem"), Some("/k.pem"), None)
            .unwrap()
            .unwrap();
        assert!(with_identity.client_identity().is_some());
    }

    #[test]
    fn tunneled_connections_verify_the_original_host() {
        let full = ssl_opts("verify_identity", None, None, None, Some("db.example"))
            .unwrap()
            .unwrap();
        assert_eq!(full.tls_hostname_override(), Some("db.example"));
        let direct = ssl_opts("verify_identity", None, None, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(direct.tls_hostname_override(), None);
    }
}
