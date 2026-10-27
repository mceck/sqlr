#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these helpers"
)]

use sqlr_core::{ConnectionConfig, Secrets};
use sqlr_testkit::{TestLock, service, unique_name};

/// The test's Postgres server. The lock is only held by the server-global tests.
pub struct PgServer {
    _lock: Option<TestLock>,
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
        agent_access: Default::default(),
    }
}

pub fn secrets(password: &str) -> Secrets {
    Secrets([("password".to_string(), password.to_string())].into())
}

/// A fixed Postgres server with a fresh database for this test.
pub async fn pg() -> (PgServer, ConnectionConfig, Secrets) {
    let (host, port) = service("POSTGRES");
    let database = unique_name("sqlr_pg");
    create_database(&host, port, &database).await;
    let mut cfg = config(&host, port);
    cfg.values.insert("database".into(), database);
    (PgServer { _lock: None }, cfg, secrets("postgres"))
}

/// The fixed global server for tests that inspect roles, sessions, locks or the database
/// list. It is reset and serialized across nextest processes.
pub async fn pg_isolated() -> (PgServer, ConnectionConfig, Secrets) {
    let (host, port) = service("POSTGRES_GLOBAL");
    let lock = TestLock::acquire("postgres-global");
    reset(&host, port).await;
    (
        PgServer { _lock: Some(lock) },
        config(&host, port),
        secrets("postgres"),
    )
}

/// The global server with TLS on, plus the CA certificate generated for it.
pub async fn pg_with_tls() -> (PgServer, ConnectionConfig, Secrets, String) {
    let (guard, mut cfg, sec) = pg_isolated().await;
    // The certificate names localhost; tests also connect to 127.0.0.1 to check that
    // verify-full uses the host name.
    cfg.values.insert("host".into(), "localhost".into());
    let ca = std::fs::read_to_string(sqlr_testkit::ca_path())
        .expect("run scripts/test-services.sh up to generate the TLS certificate");
    (guard, cfg, sec, ca)
}

async fn client(host: &str, port: u16) -> tokio_postgres::Client {
    let (client, connection) = tokio_postgres::Config::new()
        .host(host)
        .port(port)
        .user("postgres")
        .password("postgres")
        .dbname("postgres")
        .connect(tokio_postgres::NoTls)
        .await
        .expect("connect to the fixed postgres");
    tokio::spawn(connection);
    client
}

async fn create_database(host: &str, port: u16, database: &str) {
    let client = client(host, port).await;
    client
        .batch_execute(&format!(
            r#"CREATE DATABASE "{}" TEMPLATE template0"#,
            quote(database)
        ))
        .await
        .expect("create the test database");
}

fn quote(ident: &str) -> String {
    ident.replace('"', "\"\"")
}

/// Drops the databases, roles and public schema the server-global tests may have left.
async fn reset(host: &str, port: u16) {
    let client = client(host, port).await;
    let databases: Vec<String> = client
        .query(
            "SELECT datname FROM pg_database \
             WHERE datname NOT IN ('postgres', 'template0', 'template1') AND datallowconn",
            &[],
        )
        .await
        .expect("list databases")
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    for database in databases {
        client
            .batch_execute(&format!(
                r#"DROP DATABASE "{}" WITH (FORCE)"#,
                quote(&database)
            ))
            .await
            .unwrap_or_else(|e| panic!("drop database {database}: {e:?}"));
    }
    let schemas: Vec<String> = client
        .query(
            "SELECT nspname FROM pg_namespace \
             WHERE nspname NOT IN ('public', 'information_schema', 'pg_catalog', 'pg_toast') \
               AND nspname NOT LIKE 'pg\\_%'",
            &[],
        )
        .await
        .expect("list schemas")
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    for schema in schemas {
        client
            .batch_execute(&format!(r#"DROP SCHEMA "{}" CASCADE"#, quote(&schema)))
            .await
            .unwrap_or_else(|e| panic!("drop schema {schema}: {e:?}"));
    }
    let roles: Vec<String> = client
        .query(
            "SELECT rolname FROM pg_roles \
             WHERE rolname <> 'postgres' AND rolname NOT LIKE 'pg\\_%'",
            &[],
        )
        .await
        .expect("list roles")
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    for role in roles {
        let role = quote(&role);
        client
            .batch_execute(&format!(r#"DROP OWNED BY "{role}""#))
            .await
            .unwrap_or_else(|e| panic!("drop owned by {role}: {e:?}"));
        client
            .batch_execute(&format!(r#"DROP ROLE "{role}""#))
            .await
            .unwrap_or_else(|e| panic!("drop role {role}: {e:?}"));
    }
    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS public CASCADE; \
             CREATE SCHEMA public; \
             GRANT ALL ON SCHEMA public TO postgres; \
             GRANT USAGE ON SCHEMA public TO PUBLIC",
        )
        .await
        .expect("reset the public schema");
}

/// Waits until a server takes connections. The fixed services are healthchecked before
/// the test run; this remains useful when a test starts against a manually managed host.
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
