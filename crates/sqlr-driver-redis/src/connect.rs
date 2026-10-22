//! Connection setup, the connection pool, reconnection and cancellation.
//!
//! One `RedisConnection` keeps a lazily opened multiplexed connection per logical database
//! and role: the browser's connections never wait behind a console command that blocks
//! (`BLPOP`), and the console's connections can be killed to cancel it. A connection the
//! server dropped is opened again by the next call; idempotent reads then run once more.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use redis::aio::MultiplexedConnection;
use redis::{
    AsyncConnectionConfig, Client, ConnectionAddr, IntoConnectionInfo, ProtocolVersion,
    RedisConnectionInfo, RedisResult, TlsCertificates, Value,
};
use sqlr_core::{
    CancelHandle, Connection, ConnectionConfig, Driver, DriverError, DriverManifest, ErrorKind,
    KeyInfo, KeyValueConn, KvCursor, KvEdit, KvPage, Node, NodePath, RespValue, Secrets,
    ServerInfo, StatementClass, read_only_error,
};

use crate::errors::{cluster_refused, is_connection_error, map_error};
use crate::{browse, classify, console, manifest, values};

/// How long opening a connection may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a browsing command may take; console commands have no limit (`BLPOP`).
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct RedisDriver {
    manifest: DriverManifest,
}

impl RedisDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            manifest: manifest::manifest(),
        })
    }
}

/// What a connection is used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Role {
    /// Scans, value pages and edits: short commands.
    Browse,
    /// Console commands: may block, may be cancelled.
    Console,
}

/// Where and how to connect. It holds the password, so it has no `Debug`.
struct Target {
    addr: ConnectionAddr,
    username: Option<String>,
    password: Option<String>,
    /// PEM of the CA that signs the server's certificate; `None` trusts the system's.
    tls_root: Option<Vec<u8>>,
}

pub(crate) struct Inner {
    target: Target,
    /// RESP3 gives replies their types; a server before 6.0 gets RESP2.
    resp3: AtomicBool,
    slots: tokio::sync::Mutex<HashMap<(u32, Role), MultiplexedConnection>>,
    /// `CLIENT ID`s of the console connections, to kill them on cancel.
    console_ids: Mutex<Vec<i64>>,
    /// Bumped by every cancel, so a command killed by it reports `Cancelled`.
    cancel_epoch: AtomicU64,
    pub(crate) configured_db: u32,
    pub(crate) separator: Vec<u8>,
    pub(crate) databases: AtomicU64,
    read_only: AtomicBool,
}

impl Inner {
    fn client(&self, db: u32) -> RedisResult<Client> {
        let mut redis = RedisConnectionInfo::default()
            .set_db(i64::from(db))
            .set_skip_set_lib_name()
            .set_protocol(if self.resp3.load(Ordering::SeqCst) {
                ProtocolVersion::RESP3
            } else {
                ProtocolVersion::RESP2
            });
        if let Some(user) = &self.target.username {
            redis = redis.set_username(user);
        }
        if let Some(password) = &self.target.password {
            redis = redis.set_password(password);
        }
        let info = self
            .target
            .addr
            .clone()
            .into_connection_info()?
            .set_redis_settings(redis);
        match &self.target.tls_root {
            Some(root) => Client::build_with_tls(
                info,
                TlsCertificates {
                    client_tls: None,
                    root_cert: Some(root.clone()),
                },
            ),
            None => Client::open(info),
        }
    }

    async fn open(&self, db: u32, role: Role) -> Result<MultiplexedConnection, DriverError> {
        let timeout = match role {
            Role::Browse => Some(RESPONSE_TIMEOUT),
            Role::Console => None,
        };
        let config = AsyncConnectionConfig::new()
            .set_connection_timeout(Some(CONNECT_TIMEOUT))
            .set_response_timeout(timeout);
        let mut attempt = 0;
        let mut con = loop {
            let client = self.client(db).map_err(map_error)?;
            match client
                .get_multiplexed_async_connection_with_config(&config)
                .await
            {
                Ok(con) => break con,
                // A server before 6.0 does not know `HELLO 3`.
                Err(e)
                    if e.kind() == redis::ErrorKind::RESP3NotSupported
                        && self.resp3.swap(false, Ordering::SeqCst)
                        && attempt == 0 =>
                {
                    attempt += 1;
                }
                Err(e) => return Err(map_error(e)),
            }
        };
        if role == Role::Console
            && let Ok(id) = redis::cmd("CLIENT")
                .arg("ID")
                .query_async::<i64>(&mut con)
                .await
        {
            self.console_ids.lock().unwrap().push(id);
        }
        Ok(con)
    }

    /// A connection that nothing else uses, for commands that keep state on it (`WATCH`).
    pub(crate) async fn open_private(&self, db: u32) -> Result<MultiplexedConnection, DriverError> {
        self.open(db, Role::Browse).await
    }

    async fn connection(&self, db: u32, role: Role) -> Result<MultiplexedConnection, DriverError> {
        let mut slots = self.slots.lock().await;
        if let Some(con) = slots.get(&(db, role)) {
            return Ok(con.clone());
        }
        let con = self.open(db, role).await?;
        slots.insert((db, role), con.clone());
        Ok(con)
    }

    async fn evict(&self, db: u32, role: Role) {
        self.slots.lock().await.remove(&(db, role));
    }

    /// Runs `f` on the connection for `(db, role)`. When the connection turns out to be
    /// dead, the next call opens a new one; with `retry`, this call does it too and runs
    /// `f` once more (only for commands that are safe to repeat).
    pub(crate) async fn with<R, F, Fut>(
        &self,
        db: u32,
        role: Role,
        retry: bool,
        f: F,
    ) -> Result<R, DriverError>
    where
        F: Fn(MultiplexedConnection) -> Fut + Send,
        Fut: Future<Output = RedisResult<R>> + Send,
    {
        let epoch = self.cancel_epoch.load(Ordering::SeqCst);
        let mut attempt = 0;
        loop {
            let con = self.connection(db, role).await?;
            match f(con).await {
                Ok(value) => return Ok(value),
                Err(e) if is_connection_error(&e) => {
                    self.evict(db, role).await;
                    if self.cancel_epoch.load(Ordering::SeqCst) != epoch {
                        return Err(DriverError::new(
                            ErrorKind::Cancelled,
                            "The command was cancelled",
                        ));
                    }
                    if retry && attempt == 0 {
                        attempt += 1;
                        continue;
                    }
                    return Err(map_error(e));
                }
                Err(e) => return Err(map_error(e)),
            }
        }
    }

    pub(crate) fn is_read_only(&self) -> bool {
        self.read_only.load(Ordering::SeqCst)
    }

    /// Refuses a write in a read-only session.
    pub(crate) fn check_write(&self) -> Result<(), DriverError> {
        if self.is_read_only() {
            Err(read_only_error())
        } else {
            Ok(())
        }
    }

    /// Refuses a console command that may write in a read-only session.
    pub(crate) fn check_command(&self, args: &[Vec<u8>]) -> Result<(), DriverError> {
        if self.is_read_only() && classify::classify(args).modifies() {
            return Err(read_only_error());
        }
        Ok(())
    }

    async fn cancel(&self) -> Result<(), DriverError> {
        self.cancel_epoch.fetch_add(1, Ordering::SeqCst);
        let ids = std::mem::take(&mut *self.console_ids.lock().unwrap());
        if ids.is_empty() {
            return Ok(());
        }
        // The console's connection may be the one that is blocked: kill it from another.
        let mut control = self.open(self.configured_db, Role::Browse).await?;
        for id in ids {
            let _ = redis::cmd("CLIENT")
                .arg("KILL")
                .arg("ID")
                .arg(id)
                .query_async::<Value>(&mut control)
                .await;
        }
        self.slots
            .lock()
            .await
            .retain(|(_, role), _| *role != Role::Console);
        Ok(())
    }
}

#[async_trait::async_trait]
impl Driver for RedisDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        let m = &self.manifest;
        let value = |key: &str| cfg.value(m, key).map(str::trim).filter(|v| !v.is_empty());
        let invalid = |message: String| DriverError::new(ErrorKind::Other, message);
        let host =
            value("host").ok_or_else(|| invalid("missing connection field `host`".into()))?;
        let port: u16 = value("port")
            .unwrap_or("6379")
            .parse()
            .map_err(|_| invalid("invalid port".into()))?;
        let db: u32 = value("database")
            .unwrap_or("0")
            .parse()
            .map_err(|_| invalid("the database must be a number".into()))?;
        let (addr, tls_root) = match value("tls").unwrap_or("disabled") {
            "disabled" => (ConnectionAddr::Tcp(host.to_string(), port), None),
            mode @ ("required" | "allow_invalid") => {
                // rustls needs a process-wide crypto provider; another crate may have set it.
                let _ = rustls::crypto::ring::default_provider().install_default();
                let root = value("tls_ca")
                    .map(|path| {
                        std::fs::read(path)
                            .map_err(|e| invalid(format!("cannot read the CA certificate: {e}")))
                    })
                    .transpose()?;
                (
                    ConnectionAddr::TcpTls {
                        host: host.to_string(),
                        port,
                        insecure: mode == "allow_invalid",
                        tls_params: None,
                    },
                    root,
                )
            }
            other => return Err(invalid(format!("unknown TLS mode `{other}`"))),
        };
        let inner = Arc::new(Inner {
            target: Target {
                addr,
                username: value("user").map(String::from),
                password: secrets.0.get("password").cloned().filter(|p| !p.is_empty()),
                tls_root,
            },
            resp3: AtomicBool::new(true),
            slots: tokio::sync::Mutex::new(HashMap::new()),
            console_ids: Mutex::new(Vec::new()),
            cancel_epoch: AtomicU64::new(0),
            configured_db: db,
            separator: value("separator").unwrap_or(":").as_bytes().to_vec(),
            databases: AtomicU64::new(16),
            read_only: AtomicBool::new(value("read_only") == Some("true")),
        });
        // Opening checks the address, the TLS setup and the credentials.
        inner.connection(db, Role::Browse).await?;
        let cluster = inner
            .with(db, Role::Browse, false, |mut con| async move {
                let con = &mut con;
                redis::cmd("INFO")
                    .arg("cluster")
                    .query_async::<Value>(con)
                    .await
            })
            .await
            .ok()
            .and_then(|v| values::text_of(&v));
        if cluster.is_some_and(|info| info.contains("cluster_enabled:1")) {
            return Err(cluster_refused());
        }
        // Managed servers may refuse `CONFIG`; 16 is the default.
        let databases = inner
            .with(db, Role::Browse, false, |mut con| async move {
                let con = &mut con;
                redis::cmd("CONFIG")
                    .arg("GET")
                    .arg("databases")
                    .query_async::<Value>(con)
                    .await
            })
            .await
            .ok()
            .and_then(|reply| match reply {
                // RESP3 answers a map, RESP2 a flat array: the value comes second.
                Value::Map(pairs) => pairs.first().and_then(|(_, v)| values::text_of(v)),
                Value::Array(items) => items.get(1).and_then(values::text_of),
                _ => None,
            })
            .and_then(|n| n.parse::<u64>().ok());
        if let Some(n) = databases {
            inner.databases.store(n, Ordering::SeqCst);
        }
        Ok(Arc::new(RedisConnection { inner }))
    }
}

pub struct RedisConnection {
    inner: Arc<Inner>,
}

struct RedisCancel(Arc<Inner>);

#[async_trait::async_trait]
impl CancelHandle for RedisCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        self.0.cancel().await
    }
}

#[async_trait::async_trait]
impl Connection for RedisConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let db = self.inner.configured_db;
        let info = self
            .inner
            .with(db, Role::Browse, true, |mut con| async move {
                let con = &mut con;
                redis::cmd("INFO")
                    .arg("server")
                    .query_async::<Value>(con)
                    .await
            })
            .await?;
        let text = values::text_of(&info).unwrap_or_default();
        let field = |name: &str| {
            text.lines()
                .find_map(|l| l.trim().strip_prefix(name)?.strip_prefix(':'))
                .map(|v| v.trim().to_string())
        };
        let product = match field("server_name").as_deref() {
            Some("valkey") => "Valkey",
            _ => "Redis",
        };
        Ok(ServerInfo {
            product: product.into(),
            version: field("redis_version").unwrap_or_else(|| "?".into()),
        })
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        browse::browse(&self.inner, path).await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(RedisCancel(self.inner.clone()))
    }

    fn key_value(&self) -> Option<&dyn KeyValueConn> {
        Some(self)
    }

    fn database_name(&self) -> Option<&str> {
        None
    }
}

#[async_trait::async_trait]
impl KeyValueConn for RedisConnection {
    async fn scan(
        &self,
        db: u32,
        pattern: &[u8],
        cursor: u64,
        count: usize,
    ) -> Result<(Vec<KeyInfo>, u64), DriverError> {
        browse::scan(&self.inner, db, pattern, cursor, count).await
    }

    async fn key_info(&self, db: u32, key: &[u8]) -> Result<Option<KeyInfo>, DriverError> {
        values::key_info(&self.inner, db, key).await
    }

    async fn page(
        &self,
        db: u32,
        key: &[u8],
        from: KvCursor,
        count: usize,
    ) -> Result<KvPage, DriverError> {
        values::page(&self.inner, db, key, from, count).await
    }

    async fn edit(&self, db: u32, key: &[u8], edit: KvEdit) -> Result<(), DriverError> {
        values::edit(&self.inner, db, key, edit).await
    }

    async fn command(&self, db: u32, args: Vec<Vec<u8>>) -> Result<RespValue, DriverError> {
        console::command(&self.inner, db, args).await
    }

    async fn exec_block(
        &self,
        db: u32,
        commands: Vec<Vec<Vec<u8>>>,
    ) -> Result<RespValue, DriverError> {
        console::exec_block(&self.inner, db, commands).await
    }

    fn classify(&self, args: &[Vec<u8>]) -> StatementClass {
        classify::classify(args)
    }

    fn databases(&self) -> u32 {
        u32::try_from(self.inner.databases.load(Ordering::SeqCst)).unwrap_or(16)
    }

    fn set_read_only(&self, on: bool) {
        self.inner.read_only.store(on, Ordering::SeqCst);
    }
}
