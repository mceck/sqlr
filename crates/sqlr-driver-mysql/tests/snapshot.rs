//! Schema snapshots and compare against MySQL 8.4 and MariaDB 11.4.

#[allow(dead_code)]
mod support;

use std::collections::HashSet;
use std::sync::Arc;

use sqlr_core::{
    Connection, Driver, Execution, ObjectDef, ObjectKind, ObjectRef, SchemaSnapshot, compare,
    migration_script,
};
use sqlr_driver_mysql::MysqlDriver;
use support::{Server, start};

struct Env {
    _container: support::Container,
    conn: Arc<dyn Connection>,
}

impl Env {
    async fn new(server: Server, setup: &str) -> Self {
        let (container, cfg, secrets) = start(server).await;
        let conn = MysqlDriver::new().connect(&cfg, &secrets).await.unwrap();
        let env = Self {
            _container: container,
            conn,
        };
        for statement in setup.split(";\n").map(str::trim).filter(|s| !s.is_empty()) {
            env.run(statement).await;
        }
        env
    }

    async fn run(&self, sql: &str) {
        let rel = self.conn.relational().unwrap();
        match rel.execute(sql).await {
            Ok(Execution::Rows(mut s)) => {
                while s.next_chunk(1000).await.unwrap().is_some() {}
                s.close().await.unwrap();
            }
            Ok(Execution::Affected(_)) => {}
            Err(e) => panic!("{sql}\n{e:?}"),
        }
    }

    /// Runs a migration script: statements are separated by blank lines.
    async fn run_migration(&self, script: &str) {
        for sql in script.split("\n\n").map(str::trim) {
            if sql
                .lines()
                .all(|l| l.trim().is_empty() || l.starts_with("--"))
            {
                continue;
            }
            self.run(sql).await;
        }
    }

    async fn snapshot(&self, db: &str) -> SchemaSnapshot {
        self.conn
            .relational()
            .unwrap()
            .introspect()
            .unwrap()
            .load_schema(None, db)
            .await
            .unwrap()
    }
}

/// Databases `a` (the source) and `b` (the target).
const SETUP: &str = "
CREATE DATABASE a;
CREATE DATABASE b;
CREATE TABLE a.users (id INT AUTO_INCREMENT PRIMARY KEY, email VARCHAR(100) NOT NULL, created DATETIME DEFAULT CURRENT_TIMESTAMP, UNIQUE KEY uq_email (email));
CREATE TABLE a.orders (id INT PRIMARY KEY, user_id INT, total DECIMAL(10,2), KEY orders_user (user_id), CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES a.users(id) ON DELETE CASCADE);
CREATE TABLE a.fresh (id INT PRIMARY KEY, note TEXT);
CREATE VIEW a.big_orders AS SELECT id, total FROM a.orders WHERE total > 100;
CREATE TRIGGER a.fresh_note BEFORE INSERT ON a.fresh FOR EACH ROW SET NEW.note = 'x';
CREATE FUNCTION a.double_it(x INT) RETURNS INT DETERMINISTIC RETURN x * 2;
CREATE TABLE b.users (id INT AUTO_INCREMENT PRIMARY KEY, email VARCHAR(100));
CREATE TABLE b.orders (id INT PRIMARY KEY, user_id INT, total DECIMAL(10,2));
CREATE TABLE b.legacy (id INT PRIMARY KEY);
";

async fn round_trip(server: Server) {
    let env = Env::new(server, SETUP).await;
    let snap = env.snapshot("a").await;
    assert_eq!(snap.engine, "mysql");
    let kinds: HashSet<ObjectKind> = snap.objects.iter().map(|o| o.kind()).collect();
    for kind in [ObjectKind::Table, ObjectKind::View, ObjectKind::Function] {
        assert!(kinds.contains(&kind), "{kind:?}");
    }
    let introspect = env.conn.relational().unwrap().introspect().unwrap();
    for object in &snap.objects {
        let r = object.object_ref();
        let loaded = introspect
            .load_object(&ObjectRef {
                schema: Some("a".into()),
                ..r.clone()
            })
            .await
            .unwrap_or_else(|e| panic!("{r:?}: {e:?}"));
        assert_eq!(&loaded, object, "{r:?}");
    }
    let fresh = snap
        .objects
        .iter()
        .find_map(|o| match o {
            ObjectDef::Table(t) if t.table.name == "fresh" => Some(t),
            _ => None,
        })
        .unwrap();
    assert_eq!(fresh.triggers.len(), 1);
}

async fn converges(server: Server) {
    let env = Env::new(server, SETUP).await;
    let rel = env.conn.relational().unwrap();
    let generator = rel.dialect().ddl_gen().unwrap();
    let diffs = compare(
        &env.snapshot("a").await,
        &env.snapshot("b").await,
        generator,
    );
    let names: Vec<_> = diffs.iter().map(|d| d.object.name.as_str()).collect();
    for expected in [
        "big_orders",
        "double_it",
        "fresh",
        "legacy",
        "orders",
        "users",
    ] {
        assert!(names.contains(&expected), "{expected} in {names:?}");
    }
    let script = migration_script(&diffs, &HashSet::<ObjectRef>::new(), generator, "-- test");
    assert!(!script.contains("cannot generate"), "{script}");
    assert!(
        !script.contains("`a`."),
        "the target's views must not read the source database:\n{script}"
    );
    env.run_migration(&script).await;
    let again = compare(
        &env.snapshot("a").await,
        &env.snapshot("b").await,
        generator,
    );
    assert_eq!(again, Vec::new(), "after the script:\n{script}");
}

#[tokio::test(flavor = "multi_thread")]
async fn mysql84_snapshot_round_trip_equals_load_object() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    round_trip(Server::MySql).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mysql84_compare_then_apply_script_converges() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    converges(Server::MySql).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mariadb114_compare_then_apply_script_converges() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    converges(Server::MariaDb).await;
}
