#![allow(dead_code)] // each test binary uses a different subset of these helpers

use mysql_async::prelude::Queryable;
use sqlr_core::{ConnectionConfig, Secrets};
use sqlr_testkit::{test_service, unique_name};
use testcontainers_modules::mariadb::Mariadb;
use testcontainers_modules::mysql::Mysql;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

/// The servers the suite runs against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Server {
    MySql,
    MariaDb,
}

impl Server {
    pub fn product(self) -> &'static str {
        match self {
            Server::MySql => "MySQL",
            Server::MariaDb => "MariaDB",
        }
    }

    fn env_name(self) -> &'static str {
        match self {
            Server::MySql => "MYSQL",
            Server::MariaDb => "MARIADB",
        }
    }
}

/// A running test server; dropping a container stops it. `Shared` is a long-lived
/// service from `docker-compose.test.yml`.
#[allow(dead_code)] // held only to keep the container running
pub enum Container {
    MySql(ContainerAsync<Mysql>),
    MariaDb(ContainerAsync<Mariadb>),
    Shared,
}

pub fn config(host: &str, port: u16) -> ConnectionConfig {
    ConnectionConfig {
        id: "t".into(),
        name: "test".into(),
        driver: "mysql".into(),
        group: None,
        color: None,
        values: [
            ("host", host.to_string()),
            ("port", port.to_string()),
            ("database", "test".into()),
            ("user", "root".into()),
            ("show_all_databases", "true".into()),
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

/// A server for a test that only needs its own database. With a shared service every
/// test gets a fresh database; otherwise each test gets its own container.
pub async fn start(server: Server) -> (Container, ConnectionConfig, Secrets) {
    match test_service(server.env_name()) {
        Some((host, port)) => {
            let database = unique_name("sqlr_mysql");
            create_database(&host, port, &database).await;
            let (mut cfg, sec) = (config(&host, port), Secrets::default());
            cfg.values.insert("database".into(), database);
            (Container::Shared, cfg, sec)
        }
        None => start_isolated(server).await,
    }
}

/// Always starts a dedicated container: for tests that create databases, list them, or
/// change the server configuration.
pub async fn start_isolated(server: Server) -> (Container, ConnectionConfig, Secrets) {
    start_with(server, &[]).await
}

/// Like [`start_isolated`], with TLS turned off on the server.
pub async fn start_without_tls(server: Server) -> (Container, ConnectionConfig, Secrets) {
    let args: &[&str] = match server {
        Server::MySql => &["--tls-version="],
        Server::MariaDb => &["--skip-ssl"],
    };
    start_with(server, args).await
}

async fn start_with(server: Server, args: &[&str]) -> (Container, ConnectionConfig, Secrets) {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let (container, host, port) = match server {
        Server::MySql => {
            let c = Mysql::default()
                .with_tag("8.4")
                .with_cmd(args)
                .start()
                .await
                .expect("start mysql container");
            let host = c.get_host().await.expect("host").to_string();
            let port = c.get_host_port_ipv4(3306).await.expect("mapped port");
            (Container::MySql(c), host, port)
        }
        Server::MariaDb => {
            let c = Mariadb::default()
                .with_tag("11.4")
                .with_cmd(args)
                .start()
                .await
                .expect("start mariadb container");
            let host = c.get_host().await.expect("host").to_string();
            let port = c.get_host_port_ipv4(3306).await.expect("mapped port");
            (Container::MariaDb(c), host, port)
        }
    };
    (container, config(&host, port), Secrets::default())
}

async fn create_database(host: &str, port: u16, database: &str) {
    let opts = mysql_async::OptsBuilder::default()
        .ip_or_hostname(host)
        .tcp_port(port)
        .user(Some("root"));
    let mut conn = mysql_async::Conn::new(opts)
        .await
        .expect("connect to the shared mysql");
    conn.query_drop(format!("CREATE DATABASE `{database}`"))
        .await
        .expect("create the test database");
    conn.disconnect().await.ok();
}
