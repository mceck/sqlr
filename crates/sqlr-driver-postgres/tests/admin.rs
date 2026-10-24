//! Administration against PostgreSQL 17: roles, grants, sessions,
//! cancel and terminate, locks, server properties, database sizes and table statistics.

use crate::support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlr_core::{
    Admin, Connection, ConnectionConfig, Driver, ErrorKind, Grant, ObjectKind, ObjectRef, Password,
    RoleDef, Secrets, TableRef,
};
use sqlr_driver_postgres::PostgresDriver;
use support::{PgServer, pg_isolated, secrets};

struct Env {
    _c: PgServer,
    cfg: ConnectionConfig,
    sec: Secrets,
    raw: tokio_postgres::Client,
    conn: Arc<dyn Connection>,
}

async fn raw(cfg: &ConnectionConfig, sec: &Secrets) -> tokio_postgres::Client {
    let (client, conn) = tokio_postgres::Config::new()
        .host(cfg.values["host"].as_str())
        .port(cfg.values["port"].parse().unwrap())
        .user("postgres")
        .password(&sec.0["password"])
        .dbname("postgres")
        .connect(tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    client
}

impl Env {
    async fn new(setup: &str) -> Self {
        let (c, cfg, sec) = pg_isolated().await;
        let raw = raw(&cfg, &sec).await;
        raw.batch_execute(setup).await.unwrap();
        let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
        Self {
            _c: c,
            cfg,
            sec,
            raw,
            conn,
        }
    }

    fn admin(&self) -> &dyn Admin {
        self.conn
            .relational()
            .unwrap()
            .admin()
            .expect("admin support")
    }

    /// Runs statements written by the admin on the raw client.
    async fn run(&self, statements: &[String]) {
        assert!(!statements.is_empty());
        for sql in statements {
            self.raw
                .batch_execute(sql)
                .await
                .unwrap_or_else(|e| panic!("{sql}\n{e:?}"));
        }
    }

    async fn role(&self, name: &str) -> Option<RoleDef> {
        self.admin()
            .roles()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.name == name)
    }

    async fn pid(client: &tokio_postgres::Client) -> String {
        let pid: i32 = client
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        pid.to_string()
    }
}

fn object(kind: ObjectKind, schema: Option<&str>, name: &str) -> ObjectRef {
    ObjectRef {
        kind,
        database: None,
        schema: schema.map(String::from),
        name: name.into(),
        parent: None,
        signature: None,
    }
}

fn grant(grantee: &str, object: &ObjectRef, privilege: &str, grantable: bool) -> Grant {
    Grant {
        grantee: grantee.into(),
        object: object.clone(),
        privilege: privilege.into(),
        grantable,
    }
}

/// Evaluates `$body` (an `Option`) until it is `Some`, for at most `$secs` seconds.
macro_rules! eventually {
    ($secs:expr, $what:expr, $body:expr) => {{
        let deadline = Instant::now() + Duration::from_secs($secs);
        loop {
            if let Some(v) = $body {
                break v;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {}", $what);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }};
}

#[tokio::test(flavor = "multi_thread")]
async fn roles_list_and_create_alter_drop() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let env = Env::new("CREATE ROLE reader NOLOGIN;").await;
    let admin = env.admin();
    let support = admin.supports();
    assert!(support.roles && support.grants && support.sessions && support.locks);
    assert!(support.properties && support.statistics && !support.hosts);
    assert_eq!(
        support.privileges_of(ObjectKind::Table),
        [
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "TRUNCATE",
            "REFERENCES",
            "TRIGGER"
        ]
    );
    assert_eq!(admin.current_user().as_deref(), Some("postgres"));

    let roles = admin.roles().await.unwrap();
    let postgres = roles.iter().find(|r| r.name == "postgres").unwrap();
    assert!(postgres.superuser && postgres.login && postgres.password.is_none());
    // Predefined roles are listed (roles can join them) and flagged.
    let monitor = roles
        .iter()
        .find(|r| r.name == "pg_monitor")
        .expect("listed");
    assert!(monitor.is_predefined());
    assert!(!postgres.is_predefined());

    // Awkward name, a password with a quote and a backslash, a membership.
    let def = RoleDef {
        name: "App \"User\"".into(),
        login: true,
        inherit: true,
        connection_limit: Some(5),
        valid_until: Some("2099-01-01 00:00:00+00".into()),
        member_of: vec!["reader".into()],
        password: Some(Password::new("it's \\ secret")),
        ..Default::default()
    };
    let create = admin.role_ddl(None, &def);
    assert!(
        create.iter().all(|s| !s.contains("********")),
        "the real password is in the statement"
    );
    env.run(&create).await;
    let loaded = env.role("App \"User\"").await.expect("created");
    assert_eq!(
        loaded,
        RoleDef {
            password: None,
            ..def.clone()
        }
    );
    // The password set is the one typed.
    let mut cfg = env.cfg.clone();
    cfg.values.insert("user".into(), "App \"User\"".into());
    PostgresDriver::new()
        .connect(&cfg, &secrets("it's \\ secret"))
        .await
        .unwrap_or_else(|e| panic!("log in as the new role: {e:?}"));
    // The preview of the same role masks it.
    let shown = admin.role_ddl(None, &def.masked()).join("\n");
    assert!(
        shown.contains("'********'") && !shown.contains("secret"),
        "{shown}"
    );

    // Rename, change flags, drop the limit, the expiry and the membership.
    let after = RoleDef {
        name: "app_user".into(),
        create_db: true,
        connection_limit: None,
        valid_until: None,
        member_of: Vec::new(),
        ..loaded.clone()
    };
    env.run(&admin.role_ddl(Some(&loaded), &after)).await;
    assert_eq!(env.role("app_user").await.as_ref(), Some(&after));
    assert!(env.role("App \"User\"").await.is_none());
    // Nothing changed: nothing to run.
    assert!(admin.role_ddl(Some(&after), &after).is_empty());

    env.run(&admin.drop_role_ddl(&after)).await;
    assert!(env.role("app_user").await.is_none());
    let _ = &env.sec;
}

#[tokio::test(flavor = "multi_thread")]
async fn grants_round_trip() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let env = Env::new(
        "CREATE SCHEMA shop; CREATE TABLE shop.\"Order Lines\" (id int); \
         CREATE SEQUENCE shop.seq; \
         CREATE FUNCTION shop.total(a int, b int) RETURNS int LANGUAGE sql AS 'SELECT a + b'; \
         CREATE ROLE clerk;",
    )
    .await;
    let admin = env.admin();
    let table = object(ObjectKind::Table, Some("shop"), "Order Lines");
    let schema = object(ObjectKind::Schema, None, "shop");
    let sequence = object(ObjectKind::Sequence, Some("shop"), "seq");
    let function = ObjectRef {
        signature: Some("a integer, b integer".into()),
        ..object(ObjectKind::Function, Some("shop"), "total")
    };
    let add = [
        grant("clerk", &table, "SELECT", false),
        grant("clerk", &table, "UPDATE", true),
        grant("clerk", &schema, "USAGE", false),
        grant("clerk", &sequence, "USAGE", false),
        grant("clerk", &function, "EXECUTE", false),
    ];
    env.run(&admin.grant_ddl(&add, &[])).await;
    let mut got = admin.grants(Some("clerk"), None).await.unwrap();
    got.sort_by(|a, b| (&a.object.name, &a.privilege).cmp(&(&b.object.name, &b.privilege)));
    let mut want = add.to_vec();
    want.sort_by(|a, b| (&a.object.name, &a.privilege).cmp(&(&b.object.name, &b.privilege)));
    assert_eq!(got, want);

    // By object: every role's privileges on it, the owner's included.
    let on_table = admin.grants(None, Some(&table)).await.unwrap();
    assert!(on_table.iter().all(|g| g.object == table), "{on_table:?}");
    assert!(on_table.contains(&grant("clerk", &table, "SELECT", false)));
    assert!(on_table.contains(&grant("postgres", &table, "DELETE", false)));

    // Revoke the grant option only, then everything.
    env.run(&admin.grant_ddl(&[], &[grant("clerk", &table, "UPDATE", true)]))
        .await;
    let on_table = admin.grants(Some("clerk"), Some(&table)).await.unwrap();
    assert!(on_table.contains(&grant("clerk", &table, "UPDATE", false)));
    env.run(&admin.grant_ddl(
        &[],
        &[
            grant("clerk", &table, "SELECT", false),
            grant("clerk", &table, "UPDATE", false),
            grant("clerk", &function, "EXECUTE", false),
        ],
    ))
    .await;
    let left = admin.grants(Some("clerk"), None).await.unwrap();
    assert!(
        left.iter()
            .all(|g| g.object != table && g.object != function),
        "{left:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_include_own_marked() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let env = Env::new("SELECT 1").await;
    let admin = env.admin();
    let own_ids = admin.session_ids();
    assert_eq!(own_ids.len(), 2, "the explorer and query clients");
    let sessions = admin.sessions().await.unwrap();
    let mut own: Vec<String> = sessions
        .iter()
        .filter(|s| s.own)
        .map(|s| s.id.clone())
        .collect();
    own.sort();
    let mut expected = own_ids.clone();
    expected.sort();
    assert_eq!(own, expected);

    let raw_pid = Env::pid(&env.raw).await;
    let other = sessions.iter().find(|s| s.id == raw_pid).expect("listed");
    assert!(!other.own);
    assert_eq!(other.user, "postgres");
    assert_eq!(other.database.as_deref(), Some("postgres"));
    assert!(other.started_ms.is_some_and(|ms| ms > 1_600_000_000_000));
    assert!(sessions.iter().all(|s| !s.state.is_empty()));
}

#[tokio::test(flavor = "multi_thread")]
async fn other_database_connections_are_own() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let env = Env::new("CREATE DATABASE other").await;
    let mut cfg = env.cfg.clone();
    cfg.values
        .insert("show_all_databases".into(), "true".into());
    let conn = PostgresDriver::new().connect(&cfg, &env.sec).await.unwrap();
    // Browsing another database opens the driver's connection to it.
    conn.browse(&sqlr_core::NodePath(vec!["other".into()]))
        .await
        .unwrap();
    let admin = conn.relational().unwrap().admin().unwrap();
    let mut ids = admin.session_ids();
    ids.sort();
    assert_eq!(ids.len(), 4, "two clients here, two on the other database");
    let mut own: Vec<String> = admin
        .sessions()
        .await
        .unwrap()
        .into_iter()
        .filter(|s| s.own)
        .map(|s| s.id)
        .collect();
    own.sort();
    assert_eq!(own, ids);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_and_terminate_other_session() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let env = Env::new("SELECT 1").await;
    let admin = env.admin();
    let other = Arc::new(raw(&env.cfg, &env.sec).await);
    let pid = Env::pid(&other).await;

    // Cancel: the statement ends, the connection stays.
    let sleeper = {
        let other = other.clone();
        tokio::spawn(async move { other.simple_query("SELECT pg_sleep(30)").await.map(|_| ()) })
    };
    let session = eventually!(
        10,
        "the sleeping query",
        admin
            .sessions()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == pid && s.query.as_deref().is_some_and(|q| q.contains("pg_sleep")))
            .filter(|s| s.state == "active")
    );
    let started = session.started_ms;
    assert!(started.is_some());
    // A session that started at another time is another session (a reused pid): refused.
    let stale = started.map(|ms| ms - 1_000);
    assert!(admin.cancel_session(&pid, stale).await.is_err());
    assert!(admin.terminate_session(&pid, stale).await.is_err());
    assert!(!other.is_closed(), "the session was left alone");
    let at = Instant::now();
    admin.cancel_session(&pid, started).await.unwrap();
    let outcome = sleeper.await.unwrap();
    assert!(outcome.is_err(), "the query was cancelled");
    assert!(at.elapsed() < Duration::from_secs(2), "{:?}", at.elapsed());
    other.simple_query("SELECT 1").await.unwrap();

    // Terminate: the connection ends.
    let sleeper = {
        let other = other.clone();
        tokio::spawn(async move { other.simple_query("SELECT pg_sleep(30)").await.map(|_| ()) })
    };
    eventually!(
        10,
        "the second sleeping query",
        admin
            .sessions()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == pid && s.state == "active")
    );
    let at = Instant::now();
    admin.terminate_session(&pid, started).await.unwrap();
    assert!(sleeper.await.unwrap().is_err());
    assert!(at.elapsed() < Duration::from_secs(2), "{:?}", at.elapsed());
    eventually!(5, "the closed connection", other.is_closed().then_some(()));

    // Ids are numbers; an unknown session is an error.
    assert_eq!(
        admin
            .cancel_session("1; SELECT 1", None)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Other
    );
    assert!(admin.terminate_session("999999", None).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn locks_show_blocking_chain() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let env =
        Env::new("CREATE TABLE t (id int PRIMARY KEY, v int); INSERT INTO t VALUES (1, 0);").await;
    let admin = env.admin();
    let a = raw(&env.cfg, &env.sec).await;
    let a_pid = Env::pid(&a).await;
    a.batch_execute("BEGIN; UPDATE t SET v = 1 WHERE id = 1")
        .await
        .unwrap();
    let b = Arc::new(raw(&env.cfg, &env.sec).await);
    let b_pid = Env::pid(&b).await;
    let blocked = {
        let b = b.clone();
        tokio::spawn(async move { b.batch_execute("UPDATE t SET v = 2 WHERE id = 1").await })
    };
    let waiting = eventually!(
        10,
        "the waiting lock",
        admin
            .locks()
            .await
            .unwrap()
            .into_iter()
            .find(|l| l.session == b_pid && !l.granted)
    );
    assert_eq!(waiting.blocked_by, std::slice::from_ref(&a_pid));
    assert!(!waiting.mode.is_empty());
    let locks = admin.locks().await.unwrap();
    assert!(
        locks
            .iter()
            .any(|l| l.session == a_pid && l.granted && l.object.as_deref() == Some("t")),
        "{locks:?}"
    );
    assert!(
        locks
            .iter()
            .filter(|l| l.granted)
            .all(|l| l.blocked_by.is_empty())
    );
    a.batch_execute("ROLLBACK").await.unwrap();
    blocked.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn properties_and_sizes() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let env = Env::new("CREATE DATABASE other").await;
    let admin = env.admin();
    let properties = admin.properties().await.unwrap();
    let max = properties
        .iter()
        .find(|p| p.name == "max_connections")
        .expect("max_connections");
    assert!(max.value.parse::<i64>().is_ok(), "{max:?}");
    assert!(max.category.is_some() && max.description.is_some() && max.source.is_some());
    let buffers = properties
        .iter()
        .find(|p| p.name == "shared_buffers")
        .unwrap();
    assert_eq!(buffers.unit.as_deref(), Some("8kB"));

    let sizes = admin.database_sizes().await.unwrap();
    for name in ["postgres", "other"] {
        assert!(
            sizes.iter().any(|(n, s)| n == name && *s > 0),
            "{name}: {sizes:?}"
        );
    }
    assert!(sizes.iter().all(|(n, _)| n != "template0"));
}

#[tokio::test(flavor = "multi_thread")]
async fn table_stats() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let env = Env::new(
        "CREATE SCHEMA shop; \
         CREATE TABLE shop.orders (id int PRIMARY KEY, note text); \
         INSERT INTO shop.orders SELECT g, repeat('x', 100) FROM generate_series(1, 1000) g; \
         ANALYZE shop.orders; \
         CREATE TABLE public.other (id int);",
    )
    .await;
    let admin = env.admin();
    let stats = admin.table_stats(Some("shop")).await.unwrap();
    assert_eq!(stats.len(), 1, "{stats:?}");
    let s = &stats[0];
    assert_eq!(
        s.table,
        TableRef {
            database: None,
            schema: Some("shop".into()),
            name: "orders".into(),
        }
    );
    assert_eq!(s.rows_estimate, Some(1000));
    let (total, table, index) = (
        s.total_bytes.unwrap(),
        s.table_bytes.unwrap(),
        s.index_bytes.unwrap(),
    );
    assert!(table > 0 && index > 0 && total >= table + index, "{s:?}");
    assert!(
        s.extra
            .iter()
            .any(|(k, v)| k == "Last analyze" && !v.is_empty()),
        "{:?}",
        s.extra
    );
    let all = admin.table_stats(None).await.unwrap();
    assert!(all.iter().any(|s| s.table.name == "other"));
    assert!(
        all.iter()
            .all(|s| s.table.schema.as_deref() != Some("pg_catalog"))
    );
}
