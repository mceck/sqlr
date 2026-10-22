//! Connection setup for the PostgreSQL driver.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use sqlr_core::{
    CancelHandle, Connection, ConnectionConfig, Driver, DriverError, DriverManifest, ErrorKind,
    Execution, Introspect, Node, NodeKind, NodePath, ObjectDef, ObjectKind, ObjectRef,
    RelationalConn, Secrets, ServerInfo, TableInfo, TableRef, Value,
};
use tokio::sync::Mutex;
use tokio_postgres::{AsyncMessage, CancelToken, Client, Config};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::dialect::PgDialect;
use crate::execute::OriginCache;
use crate::tls::{SslMode, TlsFiles};
use crate::{browse, catalog, describe, execute, introspect, manifest, map_error, tls};

pub struct PostgresDriver {
    manifest: DriverManifest,
}

impl PostgresDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            manifest: manifest::manifest(),
        })
    }
}

/// The client used for user queries.
pub(crate) struct QueryClient {
    pub(crate) client: Client,
    /// `sqlr_cursor` is open and must be ended before the next statement runs.
    pub(crate) cursor: Option<OpenCursor>,
    /// Tables behind result columns, by oid.
    pub(crate) origins: OriginCache,
}

/// The transaction that holds `sqlr_cursor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenCursor {
    /// sqlr began it for the cursor: ending the cursor ends the transaction.
    OwnTransaction,
    /// The user's transaction block: only the cursor is closed, the block stays the user's.
    UserTransaction,
}

/// Server notices of the query client, oldest first, until `take_notices` drains them.
#[derive(Default)]
struct Notices {
    kept: Vec<String>,
    dropped: usize,
}

/// A statement that raises notices in a loop must not grow memory without bound.
const MAX_NOTICES: usize = 1000;

type NoticeSink = Arc<std::sync::Mutex<Notices>>;

pub struct PgConnection {
    /// Explorer, catalog, describe, DDL, administration and ping.
    pub(crate) meta: Client,
    /// User queries; a live result stream holds this lock.
    query: Arc<Mutex<QueryClient>>,
    notices: NoticeSink,
    cancel: CancelToken,
    tls: MakeRustlsConnect,
    pub(crate) dialect: PgDialect,
    /// The server process ids of the explorer and query clients, when the server told them
    /// (administration marks them as this connection's; connecting does not need them).
    pub(crate) pids: [Option<i32>; 2],
    /// The process ids of the connections to other databases, as they open.
    other_pids: std::sync::Mutex<Vec<i32>>,
    /// Reused to open the other databases of the same server.
    pub(crate) config: Config,
    database: String,
    show_all_databases: bool,
    /// Open connections to other databases, by database name.
    databases: Mutex<HashMap<String, Arc<PgConnection>>>,
}

impl PgConnection {
    async fn open(
        pg: &Config,
        tls: &MakeRustlsConnect,
        database: String,
        show_all_databases: bool,
    ) -> Result<Self, DriverError> {
        let notices = NoticeSink::default();
        let meta = open(pg, tls, None).await?;
        let query = open(pg, tls, Some(notices.clone())).await?;
        let cancel = query.cancel_token();
        // A server without pg_backend_pid (a compatible one) still connects.
        let (meta_pid, query_pid) = tokio::join!(backend_pid(&meta), backend_pid(&query));
        let (meta_pid, query_pid) = (meta_pid.ok(), query_pid.ok());
        Ok(Self {
            meta,
            query: Arc::new(Mutex::new(QueryClient {
                client: query,
                cursor: None,
                origins: OriginCache::new(),
            })),
            notices,
            cancel,
            tls: tls.clone(),
            dialect: PgDialect,
            pids: [meta_pid, query_pid],
            other_pids: std::sync::Mutex::new(Vec::new()),
            config: pg.clone(),
            database,
            show_all_databases,
            databases: Mutex::new(HashMap::new()),
        })
    }

    /// The server process ids of this connection's own sessions: its two clients, when
    /// known, and those of its connections to other databases.
    pub(crate) fn own_pids(&self) -> Vec<i32> {
        let others = self
            .other_pids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.pids.iter().flatten().copied().chain(others).collect()
    }

    /// A connection to another database of the same server, opened on first use.
    async fn database_connection(&self, database: &str) -> Result<Arc<Self>, DriverError> {
        if database == self.database {
            return Err(DriverError::new(
                ErrorKind::Other,
                "the connection is already attached to that database",
            ));
        }
        let mut databases = self.databases.lock().await;
        if let Some(conn) = databases.get(database) {
            return Ok(conn.clone());
        }
        let mut config = self.config.clone();
        config.dbname(database);
        let conn = Arc::new(Self::open(&config, &self.tls, database.to_string(), false).await?);
        self.other_pids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(conn.pids.iter().flatten());
        databases.insert(database.to_string(), conn.clone());
        Ok(conn)
    }

    /// With all databases shown, paths start with a database: `Some((other, rest))` when it is
    /// not this connection's own, `None` with the prefix stripped otherwise.
    fn split_database(&self, path: &NodePath) -> (Option<String>, NodePath) {
        if !self.show_all_databases {
            return (None, path.clone());
        }
        match path.segments().split_first() {
            Some((database, rest)) => {
                let other = (database != &self.database).then(|| database.clone());
                (other, NodePath(rest.to_vec()))
            }
            None => (None, path.clone()),
        }
    }
}

#[async_trait::async_trait]
impl Driver for PostgresDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    fn dialect(&self) -> Option<Arc<dyn sqlr_core::Dialect>> {
        Some(Arc::new(PgDialect))
    }

    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        sqlr_core::validate_driver_properties(&self.manifest.fields, &cfg.values)?;
        let m = &self.manifest;
        let text = |key: &str| {
            cfg.value(m, key).ok_or_else(|| {
                DriverError::new(
                    ErrorKind::Other,
                    format!("missing connection field `{key}`"),
                )
            })
        };
        let optional = |key: &str| {
            cfg.value(m, key)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(String::from)
        };
        let port: u16 = text("port")?
            .parse()
            .map_err(|_| DriverError::new(ErrorKind::Other, "invalid port"))?;
        let ssl_mode = SslMode::parse(cfg.value(m, "ssl_mode").unwrap_or(""))?;
        let files = TlsFiles {
            root_cert: optional("ssl_root_cert"),
            cert: optional("ssl_cert"),
            key: optional("ssl_key"),
        };
        // Reads certificate files and the system roots.
        let tls = tokio::task::spawn_blocking(move || tls::connector(ssl_mode, &files))
            .await
            .map_err(|e| DriverError::new(ErrorKind::Other, format!("TLS setup failed: {e}")))??;

        let database = text("database")?.to_string();
        let show_all_databases = cfg
            .value(m, "show_all_databases")
            .is_some_and(|v| v == "true");
        let mut pg = Config::new();
        let host = text("host")?;
        match (cfg.tls_server_name(), host.parse::<std::net::IpAddr>()) {
            // Through an SSH tunnel: dial its address, verify TLS against the named host.
            (Some(name), Ok(addr)) => pg.host(name).hostaddr(addr),
            _ => pg.host(host),
        };
        pg.port(port)
            .dbname(&database)
            .user(text("user")?)
            .ssl_mode(ssl_mode.wire())
            .connect_timeout(Duration::from_secs(10));
        if let Some(name) = cfg.values.get("property.application_name") {
            pg.application_name(name);
        }
        let mut options = cfg
            .values
            .get("property.options")
            .cloned()
            .unwrap_or_default();
        for (property, value) in cfg.values.iter().filter(|(k, _)| {
            k.starts_with("property.")
                && !matches!(k.as_str(), "property.application_name" | "property.options")
        }) {
            let key = property.trim_start_matches("property.");
            let escaped: String = value
                .chars()
                .flat_map(|c| {
                    if c.is_whitespace() || c == '\\' {
                        vec!['\\', c]
                    } else {
                        vec![c]
                    }
                })
                .collect();
            options.push_str(&format!(" -c {key}={escaped}"));
        }
        if !options.is_empty() {
            pg.options(&options);
        }
        if let Some(password) = secrets.0.get("password") {
            pg.password(password);
        }

        Ok(Arc::new(
            PgConnection::open(&pg, &tls, database, show_all_databases).await?,
        ))
    }
}

/// Opens one client. Its connection task forwards server notices to `notices` when given.
async fn open(
    pg: &Config,
    tls: &MakeRustlsConnect,
    notices: Option<NoticeSink>,
) -> Result<Client, DriverError> {
    let (client, mut connection) = pg.connect(tls.clone()).await.map_err(connect_error)?;
    tokio::spawn(async move {
        loop {
            match std::future::poll_fn(|cx| connection.poll_message(cx)).await {
                Some(Ok(AsyncMessage::Notice(notice))) => {
                    if let Some(sink) = &notices {
                        let mut sink = sink.lock().unwrap_or_else(PoisonError::into_inner);
                        if sink.kept.len() < MAX_NOTICES {
                            sink.kept
                                .push(format!("{}: {}", notice.severity(), notice.message()));
                        } else {
                            sink.dropped += 1;
                        }
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    tracing::warn!("postgres connection ended: {e}");
                    break;
                }
                None => break,
            }
        }
    });
    Ok(client)
}

/// The server process id of `client`'s session.
async fn backend_pid(client: &Client) -> Result<i32, DriverError> {
    let row = client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .map_err(map_error)?;
    Ok(row.get(0))
}

/// Connection errors; a failed TLS negotiation is reported as a connection failure.
fn connect_error(e: tokio_postgres::Error) -> DriverError {
    let mut err = map_error(e);
    if err.message.starts_with("error performing TLS handshake") {
        err.kind = ErrorKind::Connection;
    }
    err
}

#[async_trait::async_trait]
impl Connection for PgConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let row = self
            .meta
            .query_one("SHOW server_version", &[])
            .await
            .map_err(map_error)?;
        Ok(ServerInfo {
            product: "PostgreSQL".into(),
            version: row.get(0),
        })
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        if !self.show_all_databases {
            return browse::browse(&self.meta, path).await;
        }
        if path.segments().is_empty() {
            return browse::databases(&self.meta, path, &self.database).await;
        }
        let mut nodes = match self.split_database(path) {
            (None, rest) => browse::browse(&self.meta, &rest).await?,
            (Some(database), rest) => {
                self.database_connection(&database)
                    .await?
                    .browse(&rest)
                    .await?
            }
        };
        // The inner browse builds children of the path without the database prefix, which
        // is what it expects. Reattach the prefix so the tree can expand these nodes.
        for node in &mut nodes {
            node.path = path.child(&node.name);
        }
        Ok(nodes)
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(PgCancel {
            token: self.cancel.clone(),
            tls: self.tls.clone(),
        })
    }

    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }

    fn database_name(&self) -> Option<&str> {
        Some(&self.database)
    }

    fn schema_path(&self) -> NodePath {
        if self.show_all_databases {
            NodePath(vec![self.database.clone()])
        } else {
            NodePath::root()
        }
    }

    fn database_for_path(&self, path: &NodePath) -> Option<String> {
        if !self.show_all_databases {
            return None;
        }
        path.segments()
            .first()
            .filter(|name| name.as_str() != self.database)
            .cloned()
    }

    async fn connect_database(&self, database: &str) -> Result<Arc<dyn Connection>, DriverError> {
        if !self.show_all_databases {
            return Err(DriverError::new(
                ErrorKind::Unsupported,
                "multiple databases are not supported",
            ));
        }
        let conn = self.database_connection(database).await?;
        Ok(conn)
    }
}

#[async_trait::async_trait]
impl RelationalConn for PgConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        execute::execute(self.query.clone(), sql, &[]).await
    }

    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<Execution, DriverError> {
        execute::execute(self.query.clone(), sql, params).await
    }

    /// Runs on the explorer client, so it answers while a result stream holds the query
    /// client; tables the session created but has not committed are not visible to it.
    async fn describe(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        match &table.database {
            Some(database) if self.show_all_databases && database != &self.database => {
                let table = TableRef {
                    database: None,
                    ..table.clone()
                };
                let conn = self.database_connection(database).await?;
                describe::describe(&conn.meta, &conn.dialect, &table).await
            }
            _ => describe::describe(&self.meta, &self.dialect, table).await,
        }
    }

    /// Paths are `[schema]`, `[schema, folder, name]` and
    /// `[schema, "tables", table, "indexes", index]`, after a database when all are shown.
    fn object_ref(&self, node: &Node) -> Option<ObjectRef> {
        let (database, path) = self.split_database(&node.path);
        let segs: Vec<&str> = path.segments().iter().map(String::as_str).collect();
        let object = |kind, schema: Option<&str>, name: &str, parent: Option<&str>| ObjectRef {
            kind,
            database: database.clone(),
            schema: schema.map(String::from),
            name: name.to_string(),
            parent: parent.map(String::from),
            signature: None,
        };
        match (node.kind, segs.as_slice()) {
            (NodeKind::Database, _) => Some(ObjectRef {
                database: None,
                ..object(ObjectKind::Database, None, &node.name, None)
            }),
            (NodeKind::Schema, [s]) => Some(object(ObjectKind::Schema, None, s, None)),
            (NodeKind::Table, [s, "tables", t]) => {
                Some(object(ObjectKind::Table, Some(s), t, None))
            }
            (NodeKind::View, [s, "views", v]) => Some(object(ObjectKind::View, Some(s), v, None)),
            (NodeKind::MaterializedView, [s, "materialized views", m]) => {
                Some(object(ObjectKind::MaterializedView, Some(s), m, None))
            }
            (NodeKind::Sequence, [s, "sequences", q]) => {
                Some(object(ObjectKind::Sequence, Some(s), q, None))
            }
            (NodeKind::Index, [s, "tables", t, "indexes", i]) => {
                Some(object(ObjectKind::Index, Some(s), i, Some(t)))
            }
            // The explorer names routines `name(identity arguments)`.
            (NodeKind::Function | NodeKind::Procedure, [s, "functions", f]) => {
                let (name, args) = split_signature(f)?;
                let kind = if node.kind == NodeKind::Procedure {
                    ObjectKind::Procedure
                } else {
                    ObjectKind::Function
                };
                Some(ObjectRef {
                    signature: Some(args.to_string()),
                    ..object(kind, Some(s), name, None)
                })
            }
            _ => None,
        }
    }

    async fn check_origin(&self, table: &TableRef) -> Result<(), DriverError> {
        execute::check_origin(&self.query, table).await
    }

    fn introspect(&self) -> Option<&dyn Introspect> {
        Some(self)
    }

    fn admin(&self) -> Option<&dyn sqlr_core::Admin> {
        Some(self)
    }

    async fn take_notices(&self) -> Vec<String> {
        let mut sink = self.notices.lock().unwrap_or_else(PoisonError::into_inner);
        let mut notices = std::mem::take(&mut sink.kept);
        if sink.dropped > 0 {
            notices.push(format!("({} more notices not shown)", sink.dropped));
            sink.dropped = 0;
        }
        notices
    }

    fn dialect(&self) -> &dyn sqlr_core::Dialect {
        &self.dialect
    }

    async fn catalog(&self, schema: &str) -> Result<sqlr_core::SchemaCatalog, DriverError> {
        catalog::catalog(&self.meta, schema).await
    }
}

/// `name(arguments)` split at the parenthesis that opens the trailing argument list. It is
/// found from the end, skipping double-quoted names, so a `(` in the routine's name or a
/// parenthesized type (`numeric(10,2)`, `"we(ird"`) does not mislead it.
fn split_signature(text: &str) -> Option<(&str, &str)> {
    let inner = text.strip_suffix(')')?;
    let mut depth = 0usize;
    let mut quoted = false;
    for (at, c) in inner.char_indices().rev() {
        match c {
            '"' => quoted = !quoted,
            ')' if !quoted => depth += 1,
            '(' if !quoted && depth == 0 => return Some((&inner[..at], &inner[at + 1..])),
            '(' if !quoted => depth -= 1,
            _ => {}
        }
    }
    None
}

#[async_trait::async_trait]
impl Introspect for PgConnection {
    /// Runs on the explorer client; an object of another database (when all are shown)
    /// loads through a connection to it.
    async fn load_object(&self, object: &ObjectRef) -> Result<ObjectDef, DriverError> {
        match &object.database {
            Some(database) if self.show_all_databases && database != &self.database => {
                let conn = self.database_connection(database).await?;
                let object = ObjectRef {
                    database: None,
                    ..object.clone()
                };
                introspect::load_object(&conn.meta, &object).await
            }
            _ => introspect::load_object(&self.meta, object).await,
        }
    }

    async fn list_roles(&self) -> Result<Vec<String>, DriverError> {
        introspect::roles(&self.meta).await
    }

    /// Reads a whole schema in a fixed number of queries; another database (when all are
    /// shown) goes through a connection to it.
    async fn load_schema(
        &self,
        database: Option<&str>,
        schema: &str,
    ) -> Result<sqlr_core::SchemaSnapshot, DriverError> {
        match database {
            Some(database) if self.show_all_databases && database != self.database => {
                let conn = self.database_connection(database).await?;
                crate::snapshot::load_schema(&conn.meta, schema).await
            }
            _ => crate::snapshot::load_schema(&self.meta, schema).await,
        }
    }
}

struct PgCancel {
    token: CancelToken,
    tls: MakeRustlsConnect,
}

#[async_trait::async_trait]
impl CancelHandle for PgCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        self.token
            .cancel_query(self.tls.clone())
            .await
            .map_err(map_error)
    }
}

#[cfg(test)]
mod tests {
    use super::split_signature;

    #[test]
    fn signatures_split_at_the_trailing_argument_list() {
        assert_eq!(split_signature("f(a integer)"), Some(("f", "a integer")));
        assert_eq!(split_signature("f()"), Some(("f", "")));
        assert_eq!(
            split_signature("we(ird(a numeric(10,2), b text)"),
            Some(("we(ird", "a numeric(10,2), b text"))
        );
        assert_eq!(
            split_signature("g(a \"we(ird\")"),
            Some(("g", "a \"we(ird\""))
        );
        assert_eq!(split_signature("no arguments"), None);
    }
}
