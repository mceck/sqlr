//! Connection setup, the `Connection` implementation and server-side cancellation.

use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bson::doc;
use mongodb::Client;
use mongodb::options::{ClientOptions, Credential, ServerAddress, Tls, TlsOptions};
use sqlr_core::{
    CancelHandle, Connection, ConnectionConfig, DocumentConn, Driver, DriverError, DriverManifest,
    ErrorKind, Node, NodePath, Secrets, ServerInfo,
};

use crate::errors::map_error;
use crate::{browse, manifest};

/// How long connecting and selecting a server may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct MongoDriver {
    manifest: DriverManifest,
}

impl Default for MongoDriver {
    fn default() -> Self {
        Self {
            manifest: manifest::manifest(),
        }
    }
}

impl MongoDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait::async_trait]
impl Driver for MongoDriver {
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
            .unwrap_or("27017")
            .parse()
            .map_err(|_| invalid("invalid port".into()))?;
        let database = value("database").unwrap_or("test").to_string();
        let credential = value("user").map(|user| {
            Credential::builder()
                .username(user.to_string())
                .password(secrets.0.get("password").cloned())
                .source(value("auth_source").map(String::from))
                .build()
        });
        let tls = match value("tls").unwrap_or("disabled") {
            "disabled" => None,
            mode @ ("required" | "allow_invalid") => {
                // rustls needs a process-wide crypto provider; another crate may have set it.
                let _ = rustls::crypto::ring::default_provider().install_default();
                let skip = mode == "allow_invalid";
                Some(Tls::Enabled(
                    TlsOptions::builder()
                        .ca_file_path(value("tls_ca").map(std::path::PathBuf::from))
                        .allow_invalid_certificates(skip.then_some(true))
                        .build(),
                ))
            }
            other => return Err(invalid(format!("unknown TLS mode `{other}`"))),
        };
        let replica_set = value("replica_set").map(String::from);
        let direct = replica_set.is_none() && value("direct_connection") != Some("false");
        let options = ClientOptions::builder()
            .hosts(vec![ServerAddress::Tcp {
                host: host.to_string(),
                port: Some(port),
            }])
            .credential(credential)
            .tls(tls)
            .direct_connection(direct)
            .repl_set_name(replica_set)
            .app_name("sqlr".to_string())
            .connect_timeout(CONNECT_TIMEOUT)
            .server_selection_timeout(CONNECT_TIMEOUT)
            .build();
        let client = Client::with_options(options).map_err(map_error)?;
        // Connecting is lazy: ping so a wrong address or password fails here.
        client
            .database("admin")
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(map_error)?;
        Ok(Arc::new(MongoConnection {
            client,
            configured: database.clone(),
            current: Mutex::new(database),
            read_only: AtomicBool::new(value("read_only") == Some("true")),
            running: Arc::new(Mutex::new(Vec::new())),
        }))
    }
}

pub struct MongoConnection {
    pub(crate) client: Client,
    configured: String,
    /// The database `use` switched to.
    current: Mutex<String>,
    read_only: AtomicBool,
    /// The comments tagging the operations in flight (one per statement or stream), for
    /// `killOp`.
    pub(crate) running: RunningTags,
}

impl MongoConnection {
    pub(crate) fn current_database(&self) -> String {
        self.current.lock().clone()
    }

    pub(crate) fn set_current_database(&self, name: &str) {
        *self.current.lock() = name.to_string();
    }

    pub(crate) fn is_read_only(&self) -> bool {
        self.read_only.load(Ordering::SeqCst)
    }

    pub(crate) fn set_read_only_flag(&self, on: bool) {
        self.read_only.store(on, Ordering::SeqCst);
    }
}

/// The tags of the operations a connection has in flight.
pub(crate) type RunningTags = Arc<Mutex<Vec<String>>>;

/// Marks an operation tagged `tag` as running until dropped. Each operation adds and
/// removes only its own tag, so concurrent operations never clobber each other's.
pub(crate) struct Running {
    tags: RunningTags,
    tag: String,
}

impl Running {
    pub(crate) fn start(tags: &RunningTags, tag: &str) -> Self {
        tags.lock().push(tag.to_string());
        Self {
            tags: tags.clone(),
            tag: tag.to_string(),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut tags = self.tags.lock();
        if let Some(i) = tags.iter().position(|t| *t == self.tag) {
            tags.remove(i);
        }
    }
}

/// Kills the tagged operations in flight with `killOp`, found through `$currentOp` by
/// their comments.
struct MongoCancel {
    client: Client,
    running: RunningTags,
}

#[async_trait::async_trait]
impl CancelHandle for MongoCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        let tags: Vec<bson::Bson> = self
            .running
            .lock()
            .iter()
            .map(|t| bson::Bson::String(t.clone()))
            .collect();
        if tags.is_empty() {
            return Ok(());
        }
        let admin = self.client.database("admin");
        let reply = admin
            .run_command(doc! {
                "aggregate": 1,
                "pipeline": [
                    { "$currentOp": { "allUsers": true, "idleConnections": false } },
                    { "$match": { "$or": [
                        { "command.comment": { "$in": tags.clone() } },
                        { "cursor.originatingCommand.comment": { "$in": tags.clone() } },
                        { "originatingCommand.comment": { "$in": tags } },
                    ] } },
                    { "$project": { "opid": 1 } },
                ],
                "cursor": {},
            })
            .await
            .map_err(map_error)?;
        let ops = reply
            .get_document("cursor")
            .and_then(|c| c.get_array("firstBatch"))
            .map(|b| b.to_vec())
            .unwrap_or_default();
        for op in ops {
            if let Some(opid) = op.as_document().and_then(|d| d.get("opid")) {
                admin
                    .run_command(doc! { "killOp": 1, "op": opid.clone() })
                    .await
                    .map_err(map_error)?;
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Connection for MongoConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let info = self
            .client
            .database("admin")
            .run_command(doc! { "buildInfo": 1 })
            .await
            .map_err(map_error)?;
        Ok(ServerInfo {
            product: "MongoDB".into(),
            version: info.get_str("version").unwrap_or("?").to_string(),
        })
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        browse::browse(self, path).await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(MongoCancel {
            client: self.client.clone(),
            running: self.running.clone(),
        })
    }

    fn document(&self) -> Option<&dyn DocumentConn> {
        Some(self)
    }

    fn database_name(&self) -> Option<&str> {
        Some(&self.configured)
    }
}
