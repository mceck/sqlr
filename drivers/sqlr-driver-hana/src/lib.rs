//! SAP HANA sidecar driver built on `hdbconnect_async`. It handles custom certificates,
//! cancellation, and redirects that advertise an unreachable host.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod browse;
mod cancel;
mod catalog;
mod connect;
mod dialect;
mod errors;
mod execute;
mod manifest;
mod tls;
mod types;

use std::sync::Arc;

use async_trait::async_trait;
use hdbconnect_async::Connection as HdbConnection;
use sqlr_driver_sdk::*;

pub use cancel::cancel_statement;
pub use connect::{Endpoint, endpoint};
pub use tls::pem_to_der_dir;
pub use types::{ColumnShape, column_kind};

pub struct HanaDriver {
    manifest: DriverManifest,
}

impl HanaDriver {
    pub fn new() -> Self {
        Self {
            manifest: manifest::manifest(),
        }
    }
}

impl Default for HanaDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Driver for HanaDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        let params = connect::params(cfg, &self.manifest, secrets)?;
        let conn = HdbConnection::new(params.params.clone())
            .await
            .map_err(errors::map)?;
        conn.set_fetch_size(500).await;
        let session: i64 = conn
            .query("SELECT CURRENT_CONNECTION FROM DUMMY")
            .await
            .map_err(errors::map)?
            .try_into()
            .await
            .map_err(errors::map)?;
        if let Some(schema) = cfg
            .value(&self.manifest, "schema")
            .filter(|s| !s.is_empty())
        {
            let quoted = dialect::quote(schema);
            conn.exec(format!("SET SCHEMA {quoted}"))
                .await
                .map_err(errors::map)?;
        }
        Ok(Arc::new(HanaConnection {
            conn,
            session,
            params,
            dialect: dialect::HanaDialect,
        }))
    }
}

pub struct HanaConnection {
    pub(crate) conn: HdbConnection,
    /// `CURRENT_CONNECTION`, the id `ALTER SYSTEM CANCEL SESSION` takes.
    pub(crate) session: i64,
    pub(crate) params: connect::Params,
    pub(crate) dialect: dialect::HanaDialect,
}

#[async_trait]
impl Connection for HanaConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        let version = self.conn.get_full_version_string().await;
        self.conn
            .query("SELECT 1 FROM DUMMY")
            .await
            .map_err(errors::map)?;
        Ok(ServerInfo {
            product: "SAP HANA".into(),
            version,
        })
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        browse::browse(&self.conn, path).await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(cancel::HanaCancel::new(self.params.clone(), self.session))
    }

    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }

    fn schema_path(&self) -> NodePath {
        NodePath::root()
    }
}

#[async_trait]
impl RelationalConn for HanaConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        execute::execute(&self.conn, sql).await
    }

    async fn describe(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        catalog::describe(&self.conn, table).await
    }

    async fn catalog(&self, schema: &str) -> Result<SchemaCatalog, DriverError> {
        catalog::catalog(&self.conn, schema).await
    }

    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }
}
