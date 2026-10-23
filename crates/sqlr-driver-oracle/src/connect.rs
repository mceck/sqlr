//! Connection setup for the Oracle driver (`oracle-rs`, no Oracle client libraries).

use std::sync::Arc;
use std::time::Duration;

use oracle_rs::{Config, Connection as OraConnection};
use sqlr_core::{
    CancelHandle, Connection, ConnectionConfig, Dialect, Driver, DriverError, DriverManifest,
    ErrorKind, Execution, Introspect, Node, NodeKind, NodePath, ObjectDef, ObjectRef,
    RelationalConn, Secrets, ServerInfo, TableInfo, TableRef, Value,
};

use crate::dialect::OracleDialect;
use crate::{browse, catalog, describe, execute, introspect, manifest, map_error};

pub struct OracleDriver {
    manifest: DriverManifest,
}

impl OracleDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            manifest: manifest::manifest(),
        })
    }
}

pub struct OracleConnection {
    /// The live connection; replaced when Oracle closes it (see [`Self::healed`]).
    conn: std::sync::Mutex<Arc<OraConnection>>,
    config: Config,
    service: String,
    dialect: OracleDialect,
}

#[async_trait::async_trait]
impl Driver for OracleDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    fn dialect(&self) -> Option<Arc<dyn Dialect>> {
        Some(Arc::new(OracleDialect))
    }

    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
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
        let host = text("host")?.trim().to_string();
        let port: u16 = text("port")?
            .parse()
            .map_err(|_| DriverError::new(ErrorKind::Other, "invalid port"))?;
        let service = text("service")?.trim().to_string();
        let user = text("user")?.trim().to_string();
        let password = secrets.0.get("password").cloned().unwrap_or_default();
        let by_sid = cfg.value(m, "service_type") == Some("sid");

        let mut config = if by_sid {
            Config::with_sid(host, port, service.clone(), user, password)
        } else {
            Config::new(host, port, service.clone(), user, password)
        };
        config.connect_timeout = Duration::from_secs(15);
        if cfg.value(m, "tls").is_some_and(|v| v == "true") {
            config = config.with_tls().map_err(map_error)?;
        }
        let conn = OraConnection::connect_with_config(config.clone())
            .await
            .map_err(map_error)?;
        Ok(Arc::new(OracleConnection {
            conn: std::sync::Mutex::new(Arc::new(conn)),
            config,
            service,
            dialect: OracleDialect,
        }))
    }
}

impl OracleConnection {
    fn conn(&self) -> Arc<OraConnection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The connection to use: the current one, or a fresh one when Oracle has closed it (a
    /// failed statement can leave it unusable without saying so).
    async fn live(&self) -> Arc<OraConnection> {
        let current = self.conn();
        let usable =
            !current.is_closed() && current.state().await == oracle_rs::ConnectionState::Ready;
        if usable {
            return current;
        }
        match OraConnection::connect_with_config(self.config.clone()).await {
            Ok(fresh) => {
                let fresh = Arc::new(fresh);
                *self.conn.lock().unwrap_or_else(|e| e.into_inner()) = fresh.clone();
                fresh
            }
            Err(_) => current,
        }
    }

    /// Oracle Free closes the connection on a failed statement, and the library reports that
    /// as a closed connection: the error's cause is lost and every later call fails. On such
    /// an error the connection is opened again, so the session carries on; session state (an
    /// open transaction, `ALTER SESSION` settings) does not survive, and the message says so.
    async fn healed<T>(&self, result: Result<T, DriverError>) -> Result<T, DriverError> {
        let mut error = match result {
            Err(e) if e.kind == ErrorKind::Connection => e,
            other => return other,
        };
        if let Ok(fresh) = OraConnection::connect_with_config(self.config.clone()).await {
            *self.conn.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(fresh);
            error.message = format!(
                "{} (the connection was reopened: an open transaction was lost)",
                error.message
            );
        }
        Err(error)
    }
}

#[async_trait::async_trait]
impl Connection for OracleConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let conn = self.live().await;
        self.healed(conn.ping().await.map_err(map_error)).await?;
        let info = conn.server_info().await;
        let version = if !info.version.is_empty() {
            info.version
        } else if !info.banner.is_empty() {
            info.banner
        } else {
            info.instance_name.unwrap_or_else(|| "Oracle".to_string())
        };
        Ok(ServerInfo {
            product: "Oracle".into(),
            version,
        })
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        self.healed(browse::browse(&*self.live().await, path).await)
            .await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(OracleCancel)
    }

    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }

    fn database_name(&self) -> Option<&str> {
        Some(&self.service)
    }
}

#[async_trait::async_trait]
impl RelationalConn for OracleConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        self.healed(execute::execute(self.live().await, sql, &[]).await)
            .await
    }

    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<Execution, DriverError> {
        self.healed(execute::execute(self.live().await, sql, params).await)
            .await
    }

    async fn describe(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        self.healed(describe::describe(&*self.live().await, table).await)
            .await
    }

    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }

    async fn catalog(&self, schema: &str) -> Result<sqlr_core::SchemaCatalog, DriverError> {
        self.healed(catalog::catalog(&*self.live().await, schema).await)
            .await
    }

    /// Paths are `[schema, folder, name]` and `[schema, "tables", table, "indexes", index]`.
    fn object_ref(&self, node: &Node) -> Option<ObjectRef> {
        let segs: Vec<&str> = node.path.segments().iter().map(String::as_str).collect();
        let object = |kind, schema: &str, name: &str, parent: Option<&str>| ObjectRef {
            kind,
            database: None,
            schema: Some(schema.to_string()),
            name: name.to_string(),
            parent: parent.map(String::from),
            signature: None,
        };
        match (node.kind, segs.as_slice()) {
            (NodeKind::Index, [schema, "tables", table, "indexes", index]) => Some(object(
                sqlr_core::ObjectKind::Index,
                schema,
                index,
                Some(table),
            )),
            (NodeKind::Table, [schema, "tables", name]) => {
                Some(object(sqlr_core::ObjectKind::Table, schema, name, None))
            }
            (NodeKind::View, [schema, "views", name]) => {
                Some(object(sqlr_core::ObjectKind::View, schema, name, None))
            }
            _ => None,
        }
    }

    fn introspect(&self) -> Option<&dyn Introspect> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl Introspect for OracleConnection {
    async fn load_object(&self, object: &ObjectRef) -> Result<ObjectDef, DriverError> {
        self.healed(introspect::load_object(&*self.live().await, object).await)
            .await
    }

    async fn list_roles(&self) -> Result<Vec<String>, DriverError> {
        self.healed(introspect::roles(&*self.live().await).await)
            .await
    }

    /// The connection's own database (pluggable database) only.
    async fn load_schema(
        &self,
        database: Option<&str>,
        schema: &str,
    ) -> Result<sqlr_core::SchemaSnapshot, DriverError> {
        if database.is_some_and(|d| d != self.service) {
            return Err(DriverError::new(
                ErrorKind::Unsupported,
                "multiple databases are not supported",
            ));
        }
        self.healed(introspect::load_schema(&*self.live().await, schema).await)
            .await
    }
}

struct OracleCancel;

#[async_trait::async_trait]
impl CancelHandle for OracleCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        Err(DriverError::new(
            ErrorKind::Unsupported,
            "Oracle cannot cancel a running statement without closing the connection",
        ))
    }
}
