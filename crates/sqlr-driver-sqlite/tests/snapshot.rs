//! Schema snapshots and compare against SQLite files.

use std::collections::HashSet;
use std::sync::Arc;

use sqlr_core::{
    Connection, ConnectionConfig, Driver, Execution, ObjectDef, ObjectKind, ObjectRef,
    SchemaSnapshot, Secrets, compare, migration_script,
};
use sqlr_driver_sqlite::SqliteDriver;

struct Env {
    _dir: tempfile::TempDir,
    conn: Arc<dyn Connection>,
}

impl Env {
    async fn new(setup: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ConnectionConfig {
            id: "t".into(),
            name: "test".into(),
            driver: "sqlite".into(),
            group: None,
            color: None,
            values: [
                ("path", dir.path().join("t.db").display().to_string()),
                ("create_if_missing", "true".into()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        let conn = SqliteDriver::new()
            .connect(&cfg, &Secrets::default())
            .await
            .unwrap();
        let env = Self { _dir: dir, conn };
        env.run_script(setup).await;
        env
    }

    /// Runs the statements of `script`, split where a statement ends with `;` and a line
    /// break (a trigger's own `;` is followed by a space).
    async fn run_script(&self, script: &str) {
        let rel = self.conn.relational().unwrap();
        for sql in script.split(";\n").map(str::trim).filter(|s| !s.is_empty()) {
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

    /// Runs a migration script: statements are separated by blank lines.
    async fn run_migration(&self, script: &str) {
        let rel = self.conn.relational().unwrap();
        for sql in script.split("\n\n").map(str::trim) {
            if sql
                .lines()
                .all(|l| l.trim().is_empty() || l.starts_with("--"))
            {
                continue;
            }
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

    async fn snapshot(&self) -> SchemaSnapshot {
        self.conn
            .relational()
            .unwrap()
            .introspect()
            .unwrap()
            .load_schema(None, "main")
            .await
            .unwrap()
    }
}

const SOURCE: &str = "
CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL, created TEXT);
CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id), total REAL);
CREATE TABLE fresh (id INTEGER PRIMARY KEY, note TEXT);
CREATE INDEX idx_orders_user ON orders(user_id);
CREATE VIEW big_orders AS SELECT * FROM orders WHERE total > 100;
CREATE TRIGGER fresh_note AFTER INSERT ON fresh BEGIN UPDATE fresh SET note = 'x' WHERE id = NEW.id; END;
";

const TARGET: &str = "
CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT);
CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, total REAL);
CREATE TABLE legacy (id INTEGER PRIMARY KEY);
CREATE INDEX idx_orders_total ON orders(total);
";

#[tokio::test]
async fn snapshot_round_trip_equals_load_object() {
    let env = Env::new(SOURCE).await;
    let snap = env.snapshot().await;
    assert_eq!(snap.engine, "sqlite");
    assert_eq!(snap.schema, "main");
    let mut kinds: Vec<_> = snap.objects.iter().map(|o| o.kind()).collect();
    kinds.sort_by_key(|k| k.keyword());
    assert_eq!(
        kinds,
        [
            ObjectKind::Table,
            ObjectKind::Table,
            ObjectKind::Table,
            ObjectKind::View
        ]
    );
    let introspect = env.conn.relational().unwrap().introspect().unwrap();
    for object in &snap.objects {
        let mut loaded = introspect.load_object(&object.object_ref()).await.unwrap();
        // A single view also reads the columns it yields; a snapshot keeps the named ones.
        if let (ObjectDef::View(l), ObjectDef::View(s)) = (&mut loaded, object) {
            l.columns = s.columns.clone();
        }
        assert_eq!(&loaded, object, "{:?}", object.object_ref().name);
    }
    // Indexes and triggers belong to their tables.
    let orders = snap
        .objects
        .iter()
        .find_map(|o| match o {
            ObjectDef::Table(t) if t.table.name == "orders" => Some(t),
            _ => None,
        })
        .unwrap();
    assert_eq!(orders.indexes.len(), 1);
}

#[tokio::test]
async fn normalized_ddl_not_a_difference() {
    // The same schema written with other whitespace and spelling.
    let a = Env::new(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT 'x');
         CREATE VIEW v AS SELECT id,\n   name\n FROM t;",
    )
    .await;
    let b = Env::new(
        "CREATE TABLE t (\n  id integer primary key,\n  name text not null default 'x'\n);
         CREATE VIEW v AS SELECT id, name FROM t;",
    )
    .await;
    let gen_conn = a.conn.relational().unwrap();
    let generator = gen_conn.dialect().ddl_gen().unwrap();
    assert_eq!(
        compare(&a.snapshot().await, &b.snapshot().await, generator),
        Vec::new()
    );
}

#[tokio::test]
async fn compare_then_apply_script_converges() {
    let source = Env::new(SOURCE).await;
    let target = Env::new(TARGET).await;
    let rel = target.conn.relational().unwrap();
    let generator = rel.dialect().ddl_gen().unwrap();

    let diffs = compare(
        &source.snapshot().await,
        &target.snapshot().await,
        generator,
    );
    let names: Vec<_> = diffs.iter().map(|d| d.object.name.as_str()).collect();
    assert_eq!(
        names,
        ["big_orders", "fresh", "legacy", "orders", "users"],
        "{diffs:#?}"
    );
    let script = migration_script(&diffs, &HashSet::<ObjectRef>::new(), generator, "-- test");
    assert!(script.contains("DROP TABLE"), "{script}");
    assert!(!script.contains("cannot generate"), "{script}");

    // Run on the target: the schemas are the same afterwards.
    target.run_migration(&script).await;
    let again = compare(
        &source.snapshot().await,
        &target.snapshot().await,
        generator,
    );
    assert_eq!(again, Vec::new(), "after the script:\n{script}");
}
