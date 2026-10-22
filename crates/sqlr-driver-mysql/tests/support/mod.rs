use sqlr_core::{ConnectionConfig, Secrets};
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
}

/// A running test server; dropping it stops the container.
#[allow(dead_code)] // held only to keep the container running
pub enum Container {
    MySql(ContainerAsync<Mysql>),
    MariaDb(ContainerAsync<Mariadb>),
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

/// Starts a pinned MySQL 8.4 or MariaDB 11.4 container: user `root` without a password,
/// database `test`. Both servers offer TLS with self-signed certificates.
pub async fn start(server: Server) -> (Container, ConnectionConfig, Secrets) {
    start_with(server, &[]).await
}

/// Like [`start`], with TLS turned off on the server.
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
