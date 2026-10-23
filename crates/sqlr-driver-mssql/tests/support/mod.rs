//! A SQL Server 2025 container for the integration tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlr_core::{Connection, ConnectionConfig, Driver, Secrets};
use sqlr_driver_mssql::SqlServerDriver;
use testcontainers_modules::mssql_server::MssqlServer;
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const PASSWORD: &str = "yourStrong(!)Password";

pub async fn server() -> (impl Sized, Arc<dyn Connection>) {
    let container = MssqlServer::default()
        .with_accept_eula()
        .with_tag("2025-latest")
        .start()
        .await
        .expect("start sql server container");
    let host = container.get_host().await.unwrap().to_string();
    let port = container.get_host_port_ipv4(1433).await.unwrap();
    let cfg = ConnectionConfig {
        id: "t".into(),
        name: "test".into(),
        driver: "sqlserver".into(),
        group: None,
        color: None,
        values: [
            ("host", host),
            ("port", port.to_string()),
            ("database", "master".into()),
            ("user", "sa".into()),
            ("encryption", "not_supported".into()),
            ("trust_server_certificate", "true".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        profile: None,
        save_secrets: true,
    };
    let secrets = Secrets([("password".to_string(), PASSWORD.to_string())].into());
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match SqlServerDriver::new().connect(&cfg, &secrets).await {
            Ok(conn) => return (container, conn),
            Err(e) if Instant::now() > deadline => panic!("sql server not ready: {e:?}"),
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
}
