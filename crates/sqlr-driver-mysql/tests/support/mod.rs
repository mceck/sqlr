#![allow(dead_code)] // each test binary uses a different subset of these helpers

use mysql_async::prelude::Queryable;
use sqlr_core::{ConnectionConfig, Secrets};
use sqlr_testkit::{TestLock, service, unique_name};

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

    fn global_env_name(self) -> &'static str {
        match self {
            Server::MySql => "MYSQL_GLOBAL",
            Server::MariaDb => "MARIADB_GLOBAL",
        }
    }

    fn notls_env_name(self) -> &'static str {
        match self {
            Server::MySql => "MYSQL_NOTLS",
            Server::MariaDb => "MARIADB_NOTLS",
        }
    }

    fn lock_name(self) -> &'static str {
        match self {
            Server::MySql => "mysql-global",
            Server::MariaDb => "mariadb-global",
        }
    }
}

/// A running test server. `_lock` is set for the server-global tests, which are
/// serialized across nextest's processes.
pub struct Container {
    _lock: Option<TestLock>,
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
        agent_access: Default::default(),
    }
}

pub fn secrets(password: &str) -> Secrets {
    Secrets([("password".to_string(), password.to_string())].into())
}

/// A server for a test that only needs its own database: a fixed service with a fresh
/// database, so tests run in parallel without seeing each other's objects.
pub async fn start(server: Server) -> (Container, ConnectionConfig, Secrets) {
    let (host, port) = service(server.env_name());
    let database = unique_name("sqlr_mysql");
    create_database(&host, port, &database).await;
    let mut cfg = config(&host, port);
    cfg.values.insert("database".into(), database);
    (Container { _lock: None }, cfg, Secrets::default())
}

/// The fixed global server for tests that create databases, users or roles, list
/// databases, or change server settings. Reset before every test, then serialized.
pub async fn start_isolated(server: Server) -> (Container, ConnectionConfig, Secrets) {
    global(server, server.global_env_name()).await
}

/// The fixed global server without TLS, for the test that requires TLS and must fail.
pub async fn start_without_tls(server: Server) -> (Container, ConnectionConfig, Secrets) {
    global(server, server.notls_env_name()).await
}

async fn global(server: Server, env: &str) -> (Container, ConnectionConfig, Secrets) {
    let (host, port) = service(env);
    let lock = TestLock::acquire(server.lock_name());
    reset(&host, port).await;
    (
        Container { _lock: Some(lock) },
        config(&host, port),
        Secrets::default(),
    )
}

/// Whether the server folds table names (`lower_case_table_names != 0`). A macOS data
/// directory on a case-insensitive filesystem forces this mode, unlike the Linux image.
pub async fn lower_case_table_names(cfg: &ConnectionConfig) -> bool {
    let opts = mysql_async::OptsBuilder::default()
        .ip_or_hostname(cfg.values["host"].clone())
        .tcp_port(cfg.values["port"].parse().unwrap())
        .user(Some("root"));
    let mut conn = mysql_async::Conn::new(opts)
        .await
        .expect("read lower_case_table_names");
    let folded: Option<u8> = conn
        .query_first("SELECT @@lower_case_table_names")
        .await
        .expect("lower_case_table_names");
    conn.disconnect().await.ok();
    folded.unwrap_or(0) != 0
}

async fn connection(host: &str, port: u16) -> mysql_async::Conn {
    let opts = mysql_async::OptsBuilder::default()
        .ip_or_hostname(host)
        .tcp_port(port)
        .user(Some("root"));
    mysql_async::Conn::new(opts)
        .await
        .expect("connect to the fixed mysql")
}

async fn create_database(host: &str, port: u16, database: &str) {
    let mut conn = connection(host, port).await;
    conn.query_drop(format!("CREATE DATABASE `{database}`"))
        .await
        .expect("create the test database");
    conn.disconnect().await.ok();
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
}

/// Drops every database except the system ones and every user/role except the system
/// accounts, then recreates the `test` database the global tests expect.
async fn reset(host: &str, port: u16) {
    let mut conn = connection(host, port).await;
    // One admin test changes the global sql_mode; put the server default back.
    conn.query_drop("SET GLOBAL sql_mode = DEFAULT")
        .await
        .expect("reset sql_mode");
    let schemas: Vec<String> = conn
        .query(
            "SELECT schema_name FROM information_schema.schemata \
             WHERE schema_name NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')",
        )
        .await
        .expect("list databases");
    for schema in schemas {
        conn.query_drop(format!("DROP DATABASE `{schema}`"))
            .await
            .unwrap_or_else(|e| panic!("drop database {schema}: {e:?}"));
    }
    let users: Vec<(String, String)> = conn
        .query(
            "SELECT user, host FROM mysql.user \
             WHERE user NOT IN ('root', 'mysql.sys', 'mysql.session', 'mysql.infoschema', 'mariadb.sys', 'PUBLIC')",
        )
        .await
        .expect("list users");
    for (user, host) in users {
        conn.query_drop(format!(
            "DROP USER IF EXISTS {}@{}",
            quote(&user),
            quote(&host)
        ))
        .await
        .unwrap_or_else(|e| panic!("drop user {user}@{host}: {e:?}"));
        // MariaDB stores roles in mysql.user with an empty host; `DROP USER` does not
        // remove them, while `DROP ROLE` does.
        let role = user.replace('`', "``");
        let _ = conn
            .query_drop(format!("DROP ROLE IF EXISTS `{role}`"))
            .await;
    }
    conn.query_drop("CREATE DATABASE `test`")
        .await
        .expect("recreate the test database");
    conn.disconnect().await.ok();
}
