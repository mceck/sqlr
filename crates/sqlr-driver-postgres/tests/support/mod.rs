use sqlr_core::{ConnectionConfig, Secrets};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

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

/// Starts a pinned PostgreSQL 17 container; user, password and database are all `postgres`.
pub async fn pg() -> (ContainerAsync<Postgres>, ConnectionConfig, Secrets) {
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
