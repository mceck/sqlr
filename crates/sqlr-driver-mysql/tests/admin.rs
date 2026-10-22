//! Administration against MySQL 8.4 and MariaDB 11.4: users with
//! hosts, grants, sessions, cancel and terminate, locks, variables, database sizes and
//! table statistics. Every check runs on both servers.

#[allow(dead_code)] // the TLS helpers serve the other suite
mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use mysql_async::prelude::Queryable;
use sqlr_core::{
    Admin, Connection, ConnectionConfig, Driver, ErrorKind, Execution, Grant, ObjectKind,
    ObjectRef, Password, RoleDef, TableRef,
};
use sqlr_driver_mysql::MysqlDriver;
use support::{Container, Server, secrets, start};

macro_rules! suite {
    ($($name:ident),* $(,)?) => {
        mod mysql84 {
            $(#[tokio::test(flavor = "multi_thread")]
            async fn $name() {
                if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v|v=="1") {return;}
                super::$name(super::Server::MySql).await
            })*
        }
        mod mariadb114 {
            $(#[tokio::test(flavor = "multi_thread")]
            async fn $name() {
                if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v|v=="1") {return;}
                super::$name(super::Server::MariaDb).await
            })*
        }
    };
}

suite!(
    password_with_a_backslash_under_no_backslash_escapes,
    roles_list_and_create_alter_drop,
    grants_round_trip,
    sessions_include_own_marked,
    side_connections_are_own,
    cancel_and_terminate_other_session,
    locks_show_blocking_chain,
    properties_and_sizes,
    table_stats,
);

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

struct Env {
    _c: Container,
    server: Server,
    cfg: ConnectionConfig,
    conn: Arc<dyn Connection>,
}

impl Env {
    async fn new(server: Server, setup: &[&str]) -> Self {
        let (c, cfg, sec) = start(server).await;
        let conn = MysqlDriver::new()
            .connect(&cfg, &sec)
            .await
            .unwrap_or_else(|e| panic!("connect: {e:?}"));
        let env = Self {
            _c: c,
            server,
            cfg,
            conn,
        };
        env.run(setup).await;
        env
    }

    fn admin(&self) -> &dyn Admin {
        self.conn
            .relational()
            .unwrap()
            .admin()
            .expect("admin support")
    }

    async fn run<S: AsRef<str>>(&self, statements: &[S]) {
        let rel = self.conn.relational().unwrap();
        for sql in statements {
            let sql = sql.as_ref();
            match rel.execute(sql).await {
                Ok(Execution::Rows(mut s)) => {
                    while s.next_chunk(1000).await.unwrap().is_some() {}
                    s.close().await.unwrap();
                }
                Ok(Execution::Affected(_)) => {}
                Err(e) => panic!("{sql}\n{e:?}"),
            }
        }
    }

    /// Another client connection, as root.
    async fn raw(&self) -> mysql_async::Conn {
        let opts = mysql_async::OptsBuilder::default()
            .ip_or_hostname(self.cfg.values["host"].clone())
            .tcp_port(self.cfg.values["port"].parse().unwrap())
            .user(Some("root"))
            .db_name(Some("test"));
        mysql_async::Conn::new(opts).await.unwrap()
    }

    async fn role(&self, name: &str) -> Option<RoleDef> {
        self.admin()
            .roles()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.name == name)
    }

    /// How memberships name the role `reader` on this server: MariaDB roles have no host.
    fn reader(&self) -> &'static str {
        match self.server {
            Server::MySql => "reader@%",
            Server::MariaDb => "reader",
        }
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

async fn roles_list_and_create_alter_drop(server: Server) {
    let env = Env::new(server, &["CREATE ROLE reader"]).await;
    let admin = env.admin();
    let support = admin.supports();
    assert!(support.roles && support.grants && support.sessions && support.locks);
    assert!(support.hosts, "accounts are user@host");
    assert_eq!(admin.current_user().as_deref(), Some("root"));

    let roles = admin.roles().await.unwrap();
    assert!(
        roles
            .iter()
            .any(|r| r.name == "root" && r.host.as_deref() == Some("%") && r.login),
        "{roles:?}"
    );
    let reader = roles.iter().find(|r| r.name == "reader").expect("the role");
    assert_eq!(reader.grantee(), env.reader());

    let def = RoleDef {
        name: "app `user`".into(),
        host: Some("%".into()),
        login: true,
        connection_limit: Some(3),
        member_of: vec![env.reader().into()],
        password: Some(Password::new("it's \\ secret")),
        ..Default::default()
    };
    env.run(&admin.role_ddl(None, &def)).await;
    let loaded = env.role("app `user`").await.expect("created");
    assert_eq!(loaded.host.as_deref(), Some("%"));
    assert!(loaded.login && loaded.password.is_none());
    assert_eq!(loaded.connection_limit, Some(3));
    assert_eq!(loaded.member_of, [env.reader()]);
    // The password set is the one typed.
    let mut cfg = env.cfg.clone();
    cfg.values.insert("user".into(), "app `user`".into());
    // It has no privileges on the test database.
    cfg.values.remove("database");
    MysqlDriver::new()
        .connect(&cfg, &secrets("it's \\ secret"))
        .await
        .unwrap_or_else(|e| panic!("log in as the new user: {e:?}"));

    // Rename, lock, drop the limit and the membership.
    let after = RoleDef {
        name: "app2".into(),
        login: false,
        connection_limit: None,
        member_of: Vec::new(),
        ..loaded.clone()
    };
    env.run(&admin.role_ddl(Some(&loaded), &after)).await;
    let changed = env.role("app2").await.expect("renamed");
    assert!(!changed.login);
    assert_eq!(changed.connection_limit, None);
    assert!(changed.member_of.is_empty());
    assert!(env.role("app `user`").await.is_none());
    assert!(admin.role_ddl(Some(&changed), &changed).is_empty());

    env.run(&admin.drop_role_ddl(&changed)).await;
    assert!(env.role("app2").await.is_none());
}

async fn password_with_a_backslash_under_no_backslash_escapes(server: Server) {
    let (c, cfg, sec) = start(server).await;
    let opts = mysql_async::OptsBuilder::default()
        .ip_or_hostname(cfg.values["host"].clone())
        .tcp_port(cfg.values["port"].parse().unwrap())
        .user(Some("root"));
    let mut raw = mysql_async::Conn::new(opts).await.unwrap();
    raw.query_drop("SET GLOBAL sql_mode = CONCAT(@@GLOBAL.sql_mode, ',NO_BACKSLASH_ESCAPES')")
        .await
        .unwrap();
    // Connections opened from now on take the mode.
    let conn = MysqlDriver::new().connect(&cfg, &sec).await.unwrap();
    let env = Env {
        _c: c,
        server,
        cfg,
        conn,
    };
    let def = RoleDef {
        name: "slash".into(),
        host: Some("%".into()),
        login: true,
        password: Some(Password::new("a\\b'c")),
        ..Default::default()
    };
    env.run(&env.admin().role_ddl(None, &def)).await;
    let mut cfg = env.cfg.clone();
    cfg.values.insert("user".into(), "slash".into());
    cfg.values.remove("database");
    MysqlDriver::new()
        .connect(&cfg, &secrets("a\\b'c"))
        .await
        .unwrap_or_else(|e| panic!("log in with the typed password: {e:?}"));
}

async fn grants_round_trip(server: Server) {
    let env = Env::new(
        server,
        &[
            "CREATE DATABASE shop",
            "CREATE TABLE shop.`Order Lines` (id int)",
            "CREATE FUNCTION shop.total(a int, b int) RETURNS int DETERMINISTIC RETURN a + b",
            "CREATE USER clerk@'%'",
        ],
    )
    .await;
    let admin = env.admin();
    let table = object(ObjectKind::Table, Some("shop"), "Order Lines");
    let database = object(ObjectKind::Database, None, "shop");
    let function = object(ObjectKind::Function, Some("shop"), "total");
    let add = [
        grant("clerk@%", &table, "SELECT", false),
        grant("clerk@%", &database, "INSERT", false),
        grant("clerk@%", &function, "EXECUTE", false),
    ];
    env.run(&admin.grant_ddl(&add, &[])).await;
    let got = admin.grants(Some("clerk@%"), None).await.unwrap();
    for g in &add {
        assert!(got.contains(g), "{g:?} in {got:?}");
    }
    let on_table = admin.grants(None, Some(&table)).await.unwrap();
    assert_eq!(on_table, [grant("clerk@%", &table, "SELECT", false)]);

    // MySQL's grant option covers every privilege at its level.
    env.run(&admin.grant_ddl(&[grant("clerk@%", &table, "UPDATE", true)], &[]))
        .await;
    let on_table = admin.grants(Some("clerk@%"), Some(&table)).await.unwrap();
    assert!(on_table.contains(&grant("clerk@%", &table, "UPDATE", true)));
    assert!(on_table.contains(&grant("clerk@%", &table, "SELECT", true)));
    env.run(&admin.grant_ddl(&[], &[grant("clerk@%", &table, "UPDATE", true)]))
        .await;
    let on_table = admin.grants(Some("clerk@%"), Some(&table)).await.unwrap();
    assert!(on_table.iter().all(|g| !g.grantable), "{on_table:?}");

    env.run(&admin.grant_ddl(&[], &add)).await;
    let left = admin.grants(Some("clerk@%"), None).await.unwrap();
    assert!(
        left.iter()
            .all(|g| g.object != database && g.object != function && g.privilege != "SELECT"),
        "{left:?}"
    );

    // The last grantable privilege on an object goes with the object's grant option.
    env.run(&admin.grant_ddl(&[grant("clerk@%", &table, "INSERT", true)], &[]))
        .await;
    let revoke = [
        grant("clerk@%", &table, "INSERT", false),
        grant("clerk@%", &table, "UPDATE", false),
        grant("clerk@%", &table, "INSERT", true),
    ];
    env.run(&admin.grant_ddl(&[], &revoke)).await;
    let left = admin.grants(Some("clerk@%"), Some(&table)).await.unwrap();
    assert!(left.is_empty(), "{left:?}");
    // Another account's grants are not this one's.
    assert!(
        admin
            .grants(Some("clerk@localhost"), None)
            .await
            .unwrap()
            .is_empty()
    );
}

async fn sessions_include_own_marked(server: Server) {
    let env = Env::new(server, &[] as &[&str]).await;
    let admin = env.admin();
    let mut raw = env.raw().await;
    let raw_id: u64 = raw
        .query_first("SELECT CONNECTION_ID()")
        .await
        .unwrap()
        .unwrap();
    let own_ids = admin.session_ids();
    assert_eq!(own_ids.len(), 2, "the explorer and query connections");

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
    let other = sessions
        .iter()
        .find(|s| s.id == raw_id.to_string())
        .expect("listed");
    assert!(!other.own);
    assert_eq!(other.user, "root");
    assert_eq!(other.database.as_deref(), Some("test"));
    assert!(other.is_idle(), "{other:?}");
}

async fn side_connections_are_own(server: Server) {
    let env = Env::new(server, &["CREATE DATABASE other"]).await;
    // A cancel opens the connection that runs KILL QUERY; another database opens its own.
    env.conn.cancel_handle().cancel().await.unwrap();
    env.conn.connect_database("other").await.unwrap();
    let admin = env.admin();
    let mut ids = admin.session_ids();
    ids.sort();
    assert_eq!(
        ids.len(),
        5,
        "explorer, query, kill, and the other database's two"
    );
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

async fn cancel_and_terminate_other_session(server: Server) {
    let env = Env::new(server, &[] as &[&str]).await;
    let admin = env.admin();
    let mut other = env.raw().await;
    let id: u64 = other
        .query_first("SELECT CONNECTION_ID()")
        .await
        .unwrap()
        .unwrap();
    let id = id.to_string();

    // Cancel: SLEEP ends early, the connection stays.
    let sleeper = tokio::spawn(async move {
        let r: Result<Option<i64>, _> = other.query_first("SELECT SLEEP(30)").await;
        (r.map(|_| ()), other)
    });
    eventually!(
        10,
        "the sleeping query",
        admin
            .sessions()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == id && s.query.as_deref().is_some_and(|q| q.contains("SLEEP")))
    );
    let at = Instant::now();
    admin.cancel_session(&id, None).await.unwrap();
    let (_, mut other) = sleeper.await.unwrap();
    assert!(at.elapsed() < Duration::from_secs(2), "{:?}", at.elapsed());
    let one: Option<i64> = other.query_first("SELECT 1").await.unwrap();
    assert_eq!(one, Some(1));

    // Terminate: the connection ends.
    let sleeper = tokio::spawn(async move {
        let r: Result<Option<i64>, _> = other.query_first("SELECT SLEEP(30)").await;
        let next: Result<Option<i64>, _> = other.query_first("SELECT 1").await;
        (r.map(|_| ()), next.map(|_| ()))
    });
    eventually!(
        10,
        "the second sleeping query",
        admin
            .sessions()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == id && s.query.as_deref().is_some_and(|q| q.contains("SLEEP")))
    );
    let at = Instant::now();
    admin.terminate_session(&id, None).await.unwrap();
    let (_, next) = sleeper.await.unwrap();
    assert!(next.is_err(), "the connection is gone");
    assert!(at.elapsed() < Duration::from_secs(2), "{:?}", at.elapsed());

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

async fn locks_show_blocking_chain(server: Server) {
    let env = Env::new(
        server,
        &[
            "CREATE TABLE test.t (id int PRIMARY KEY, v int) ENGINE=InnoDB",
            "INSERT INTO test.t VALUES (1, 0)",
        ],
    )
    .await;
    let admin = env.admin();
    let mut a = env.raw().await;
    let a_id: u64 = a
        .query_first("SELECT CONNECTION_ID()")
        .await
        .unwrap()
        .unwrap();
    a.query_drop("START TRANSACTION").await.unwrap();
    a.query_drop("UPDATE t SET v = 1 WHERE id = 1")
        .await
        .unwrap();
    let mut b = env.raw().await;
    let b_id: u64 = b
        .query_first("SELECT CONNECTION_ID()")
        .await
        .unwrap()
        .unwrap();
    let blocked =
        tokio::spawn(async move { b.query_drop("UPDATE t SET v = 2 WHERE id = 1").await });
    let (a_id, b_id) = (a_id.to_string(), b_id.to_string());
    let waiting = eventually!(
        10,
        "the waiting lock",
        admin
            .locks()
            .await
            .unwrap()
            .into_iter()
            .find(|l| l.session == b_id && !l.granted)
    );
    assert_eq!(waiting.blocked_by, std::slice::from_ref(&a_id));
    assert!(waiting.object.as_deref().is_some_and(|o| o.contains('t')));
    let locks = admin.locks().await.unwrap();
    assert!(
        locks.iter().any(|l| l.session == a_id && l.granted),
        "{locks:?}"
    );
    a.query_drop("ROLLBACK").await.unwrap();
    blocked.await.unwrap().unwrap();
}

async fn properties_and_sizes(server: Server) {
    let env = Env::new(
        server,
        &["CREATE DATABASE shop", "CREATE TABLE shop.t (id int)"],
    )
    .await;
    let admin = env.admin();
    let properties = admin.properties().await.unwrap();
    let max = properties
        .iter()
        .find(|p| p.name == "max_connections")
        .expect("max_connections");
    assert!(max.value.parse::<i64>().is_ok(), "{max:?}");
    assert_eq!(max.category.as_deref(), Some("max"));
    let sizes = admin.database_sizes().await.unwrap();
    assert!(
        sizes.iter().any(|(n, s)| n == "shop" && *s >= 0),
        "{sizes:?}"
    );
    assert!(
        sizes.iter().any(|(n, s)| n == "mysql" && *s > 0),
        "{sizes:?}"
    );
}

async fn table_stats(server: Server) {
    let mut setup = vec![
        "CREATE TABLE test.orders (id int PRIMARY KEY, note text, KEY by_note (note(10)))"
            .to_string(),
    ];
    for i in 0..10 {
        setup.push(format!("INSERT INTO test.orders VALUES ({i}, 'note {i}')"));
    }
    setup.push("ANALYZE TABLE test.orders".into());
    let refs: Vec<&str> = setup.iter().map(String::as_str).collect();
    let env = Env::new(server, &refs).await;
    let admin = env.admin();
    let stats = admin.table_stats(Some("test")).await.unwrap();
    let s = stats
        .iter()
        .find(|s| s.table.name == "orders")
        .expect("orders");
    assert_eq!(
        s.table,
        TableRef {
            database: None,
            schema: Some("test".into()),
            name: "orders".into(),
        }
    );
    assert!(s.rows_estimate.is_some() && s.total_bytes.is_some_and(|b| b > 0));
    assert!(s.index_bytes.is_some() && s.table_bytes.is_some());
    assert!(
        s.extra.iter().any(|(k, v)| k == "Engine" && v == "InnoDB"),
        "{s:?}"
    );
}
