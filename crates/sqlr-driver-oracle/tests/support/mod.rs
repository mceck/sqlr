//! The fixed Oracle Free server used by the integration tests.

use std::sync::Arc;

use sqlr_core::{Connection, ConnectionConfig, Driver, Secrets};
use sqlr_driver_oracle::OracleDriver;
use sqlr_testkit::service;

/// The whole schema is shared by every Oracle test, and catalog paging walks it with
/// `OFFSET`: a table created by another process between two pages would shift the rows.
/// One lock per process serializes the tests; nextest gives each test its own process.
static LOCK: std::sync::OnceLock<sqlr_testkit::TestLock> = std::sync::OnceLock::new();

pub async fn connect() -> Arc<dyn Connection> {
    let _ = LOCK.get_or_init(|| sqlr_testkit::TestLock::acquire("oracle"));
    let (host, port) = service("ORACLE");
    let cfg = ConnectionConfig {
        id: "o".into(),
        name: "oracle".into(),
        driver: "oracle".into(),
        group: None,
        color: None,
        values: [
            ("host", host),
            ("port", port.to_string()),
            ("service", "FREEPDB1".into()),
            ("service_type", "service".into()),
            ("user", "sqlr_test".into()),
            ("tls", "false".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        profile: None,
        save_secrets: true,
    };
    let secrets = Secrets([("password".to_string(), "sqlr_test".to_string())].into());
    OracleDriver::new()
        .connect(&cfg, &secrets)
        .await
        .expect("connect to the fixed Oracle")
}
