//! The bundled SQLite driver as a sidecar: the conformance suite runs against it through
//! the protocol and must match the in-process result.

fn main() -> ! {
    sqlr_driver_sdk::run(SqliteDriverShim(sqlr_driver_sqlite::SqliteDriver::new()))
}

/// `SqliteDriver::new` hands out an `Arc`; the SDK takes the driver by value.
struct SqliteDriverShim(std::sync::Arc<sqlr_driver_sqlite::SqliteDriver>);

#[async_trait::async_trait]
impl sqlr_driver_sdk::Driver for SqliteDriverShim {
    fn manifest(&self) -> &sqlr_driver_sdk::DriverManifest {
        self.0.manifest()
    }

    async fn connect(
        &self,
        cfg: &sqlr_driver_sdk::ConnectionConfig,
        secrets: &sqlr_driver_sdk::Secrets,
    ) -> Result<std::sync::Arc<dyn sqlr_driver_sdk::Connection>, sqlr_driver_sdk::DriverError> {
        self.0.connect(cfg, secrets).await
    }
}
