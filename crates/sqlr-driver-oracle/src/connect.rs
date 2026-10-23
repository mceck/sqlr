//! Connection setup for the Oracle driver (`oracle-rs`, no Oracle client libraries).

use std::sync::Arc;
use std::time::Duration;

use oracle_rs::{Config, Connection as OraConnection};
use sqlr_core::{
    CancelHandle, Connection, ConnectionConfig, Dialect, Driver, DriverError, DriverManifest,
    ErrorKind, Execution, Node, NodePath, RelationalConn, Secrets, ServerInfo, TableInfo, TableRef,
    Value,
};

use crate::dialect::OracleDialect;
use crate::{browse, catalog, describe, execute, manifest, map_error};

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
    conn: Arc<OraConnection>,
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
        let conn = OraConnection::connect_with_config(config)
            .await
            .map_err(map_error)?;
        Ok(Arc::new(OracleConnection {
            conn: Arc::new(conn),
            service,
            dialect: OracleDialect,
        }))
    }
}

#[async_trait::async_trait]
impl Connection for OracleConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        self.conn.ping().await.map_err(map_error)?;
        let info = self.conn.server_info().await;
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
        browse::browse(&self.conn, path).await
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
        execute::execute(self.conn.clone(), sql, &[]).await
    }

    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<Execution, DriverError> {
        execute::execute(self.conn.clone(), sql, params).await
    }

    async fn describe(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        describe::describe(&self.conn, table).await
    }

    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }

    async fn catalog(&self, schema: &str) -> Result<sqlr_core::SchemaCatalog, DriverError> {
        catalog::catalog(&self.conn, schema).await
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
