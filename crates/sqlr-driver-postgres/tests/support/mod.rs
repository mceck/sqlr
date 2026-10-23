#![allow(dead_code)] // each test binary uses a different subset of these helpers

use sqlr_core::{ConnectionConfig, Secrets};
use sqlr_testkit::{test_service, unique_name};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

/// The test's Postgres server: a shared service when `SQLR_TEST_POSTGRES` is set, an
/// isolated testcontainer otherwise.
pub enum PgServer {
    Container(Box<ContainerAsync<Postgres>>),
    Shared,
}

pub fn config(host: &str, port: u16) -> ConnectionConfig {
    ConnectionConfig {
        id: "t".into(),
        name: "test".into(),
        driver: "postgres".into(),
        group: None,
        color: None,
        values: [
            ("host", host.to_string()),
            ("port", port.to_string()),
            ("database", "postgres".into()),
            ("user", "postgres".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        profile: None,
        save_secrets: true,
    }
}

pub fn secrets(password: &str) -> Secrets {
    Secrets([("password".to_string(), password.to_string())].into())
}

/// A Postgres server for a test that only needs its own database. With a shared service
/// every test gets a fresh database; otherwise each test gets its own container.
pub async fn pg() -> (PgServer, ConnectionConfig, Secrets) {
    match test_service("POSTGRES") {
        Some((host, port)) => {
            let database = unique_name("sqlr_pg");
            create_database(&host, port, &database).await;
            let (mut cfg, sec) = (config(&host, port), secrets("postgres"));
            cfg.values.insert("database".into(), database);
            (PgServer::Shared, cfg, sec)
        }
        None => {
            let (container, cfg, sec) = pg_isolated().await;
            (PgServer::Container(Box::new(container)), cfg, sec)
        }
    }
}

/// Always starts a dedicated container: for tests that assert on server-global state
/// (roles, sessions, database lists) or that change the server configuration.
pub async fn pg_isolated() -> (ContainerAsync<Postgres>, ConnectionConfig, Secrets) {
    let container = Postgres::default()
        .with_tag("17-alpine")
        .start()
        .await
        .expect("start postgres container");
    let host = container.get_host().await.expect("host").to_string();
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("mapped port");
    let (cfg, sec) = (config(&host, port), secrets("postgres"));
    wait_ready(&cfg, &sec).await;
    (container, cfg, sec)
}

async fn create_database(host: &str, port: u16, database: &str) {
    let (client, connection) = tokio_postgres::Config::new()
        .host(host)
        .port(port)
        .user("postgres")
        .password("postgres")
        .dbname("postgres")
        .connect(tokio_postgres::NoTls)
        .await
        .expect("connect to the shared postgres");
    tokio::spawn(connection);
    client
        .batch_execute(&format!(
            r#"CREATE DATABASE "{database}" TEMPLATE template0"#
        ))
        .await
        .expect("create the test database");
}

/// Waits until the server takes connections. The container is reported ready on a log
/// line, but the image restarts the server once after its init scripts, so the first
/// connects can still be refused.
pub async fn wait_ready(cfg: &ConnectionConfig, sec: &Secrets) {
    use sqlr_core::Driver;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut delay = std::time::Duration::from_millis(100);
    loop {
        match sqlr_driver_postgres::PostgresDriver::new()
            .connect(cfg, sec)
            .await
        {
            Ok(_) => return,
            Err(e) if std::time::Instant::now() >= deadline => {
                panic!("postgres never took connections: {e:?}")
            }
            Err(_) => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(std::time::Duration::from_secs(1));
            }
        }
    }
}
