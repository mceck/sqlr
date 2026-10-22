//! Schema snapshots and compare against PostgreSQL 17.

mod support;

use std::collections::HashSet;
use std::sync::Arc;

use sqlr_core::{
    Connection, Driver, ObjectDef, ObjectKind, ObjectRef, SchemaSnapshot, compare, migration_script,
};
use sqlr_driver_postgres::PostgresDriver;
use support::pg;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;

struct Env {
    _c: ContainerAsync<Postgres>,
    raw: tokio_postgres::Client,
    conn: Arc<dyn Connection>,
}

impl Env {
    async fn new(setup: &str) -> Self {
        let (c, cfg, sec) = pg().await;
        let (raw, connection) = tokio_postgres::Config::new()
            .host(cfg.values["host"].as_str())
            .port(cfg.values["port"].parse().unwrap())
            .user("postgres")
            .password(&sec.0["password"])
            .dbname("postgres")
            .connect(tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        raw.batch_execute(setup).await.unwrap();
        let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
        Self { _c: c, raw, conn }
    }

    async fn snapshot(&self, schema: &str) -> SchemaSnapshot {
        self.conn
            .relational()
            .unwrap()
            .introspect()
            .unwrap()
            .load_schema(None, schema)
            .await
            .unwrap()
    }
}

/// The same objects in schemas `a` (the source) and `b` (the target), `b` behind.
const SETUP: &str = "
CREATE SCHEMA a; CREATE SCHEMA b;
CREATE TABLE a.users (id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY, email text NOT NULL UNIQUE,
  created timestamptz DEFAULT now(), CONSTRAINT email_ok CHECK (position('@' in email) > 0));
CREATE TABLE a.orders (id integer PRIMARY KEY, user_id integer REFERENCES a.users(id) ON DELETE CASCADE, total numeric(10,2));
CREATE INDEX orders_user ON a.orders (user_id);
CREATE TABLE a.fresh (id integer PRIMARY KEY, note text);
CREATE VIEW a.big_orders AS SELECT id, total FROM a.orders WHERE total > 100;
CREATE MATERIALIZED VIEW a.order_totals AS SELECT user_id, sum(total) AS s FROM a.orders GROUP BY user_id;
CREATE SEQUENCE a.tickets START 100 INCREMENT 5;
CREATE FUNCTION a.double(x integer) RETURNS integer LANGUAGE sql IMMUTABLE AS 'SELECT x * 2';
CREATE FUNCTION a.touch() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN NEW.note := now()::text; RETURN NEW; END';
CREATE TRIGGER fresh_touch BEFORE INSERT ON a.fresh FOR EACH ROW EXECUTE FUNCTION a.touch();
COMMENT ON TABLE a.users IS 'people';
CREATE TABLE b.users (id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY, email text);
CREATE TABLE b.orders (id integer PRIMARY KEY, user_id integer, total numeric(10,2));
CREATE TABLE b.legacy (id integer PRIMARY KEY);
CREATE INDEX orders_total ON b.orders (total);
";

fn by_ref(snapshot: &SchemaSnapshot) -> Vec<ObjectRef> {
    let mut refs: Vec<ObjectRef> = snapshot.objects.iter().map(|o| o.object_ref()).collect();
    refs.sort_by_key(|r| (r.kind.keyword(), r.name.clone()));
    refs
}

#[tokio::test]
async fn snapshot_round_trip_equals_load_object() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let env = Env::new(SETUP).await;
    let snap = env.snapshot("a").await;
    assert_eq!(snap.engine, "postgres");
    let kinds: HashSet<ObjectKind> = snap.objects.iter().map(|o| o.kind()).collect();
    for kind in [
        ObjectKind::Table,
        ObjectKind::View,
        ObjectKind::MaterializedView,
        ObjectKind::Sequence,
        ObjectKind::Function,
    ] {
        assert!(kinds.contains(&kind), "{kind:?} in {:?}", by_ref(&snap));
    }
    let introspect = env.conn.relational().unwrap().introspect().unwrap();
    for object in &snap.objects {
        let loaded = introspect
            .load_object(&object.object_ref())
            .await
            .unwrap_or_else(|e| panic!("{:?}: {e:?}", object.object_ref()));
        assert_eq!(&loaded, object, "{:?}", object.object_ref());
    }
    // The trigger belongs to its table.
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

#[tokio::test]
async fn compare_then_apply_script_converges() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let env = Env::new(SETUP).await;
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
        "double",
        "fresh",
        "legacy",
        "order_totals",
        "orders",
        "tickets",
        "touch",
        "users",
    ] {
        assert!(names.contains(&expected), "{expected} in {names:?}");
    }
    let script = migration_script(&diffs, &HashSet::<ObjectRef>::new(), generator, "-- test");
    assert!(!script.contains("cannot generate"), "{script}");
    env.raw
        .batch_execute(&script)
        .await
        .unwrap_or_else(|e| panic!("{e:?}\n{script}"));
    let again = compare(
        &env.snapshot("a").await,
        &env.snapshot("b").await,
        generator,
    );
    assert_eq!(again, Vec::new(), "after the script:\n{script}");
}

#[tokio::test]
async fn normalized_ddl_not_a_difference() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    // The same schema, created with other whitespace and type spellings.
    let env = Env::new(
        "CREATE SCHEMA a; CREATE SCHEMA b;
         CREATE TABLE a.t (id int4 PRIMARY KEY, name varchar(20) DEFAULT 'x');
         CREATE VIEW a.v AS SELECT id,
                 name
           FROM a.t;
         CREATE TABLE b.t (id integer PRIMARY KEY, name character varying(20) DEFAULT 'x'::character varying);
         CREATE VIEW b.v AS SELECT id, name FROM b.t;",
    )
    .await;
    let rel = env.conn.relational().unwrap();
    let generator = rel.dialect().ddl_gen().unwrap();
    assert_eq!(
        compare(
            &env.snapshot("a").await,
            &env.snapshot("b").await,
            generator
        ),
        Vec::new()
    );
}

#[tokio::test]
async fn schemas_on_the_search_path_compare_by_qualified_names() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    // `public` is on the search path: its views and serial defaults print unqualified.
    let env = Env::new(
        "CREATE SCHEMA staging;
         CREATE TABLE public.t (id serial PRIMARY KEY, name text);
         CREATE VIEW public.v AS SELECT id, name FROM public.t;
         CREATE TABLE staging.t (id serial PRIMARY KEY, name text);
         CREATE VIEW staging.v AS SELECT id, name FROM staging.t;",
    )
    .await;
    let rel = env.conn.relational().unwrap();
    let generator = rel.dialect().ddl_gen().unwrap();
    let diffs = compare(
        &env.snapshot("public").await,
        &env.snapshot("staging").await,
        generator,
    );
    assert_eq!(diffs, Vec::new(), "{diffs:#?}");
}

#[tokio::test]
async fn serial_tables_script_runs_and_converges() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    // The target has a serial table the source lacks (its sequence goes with it), and the
    // source has an identity table the target lacks (its sequence is named by the server).
    let env = Env::new(
        "CREATE SCHEMA a; CREATE SCHEMA b;
         CREATE TABLE a.fresh (id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY, n int);
         CREATE TABLE b.legacy (id serial PRIMARY KEY);",
    )
    .await;
    let rel = env.conn.relational().unwrap();
    let generator = rel.dialect().ddl_gen().unwrap();
    let diffs = compare(
        &env.snapshot("a").await,
        &env.snapshot("b").await,
        generator,
    );
    let script = migration_script(&diffs, &HashSet::<ObjectRef>::new(), generator, "");
    env.raw
        .batch_execute(&script)
        .await
        .unwrap_or_else(|e| panic!("{e:?}\n{script}"));
    let again = compare(
        &env.snapshot("a").await,
        &env.snapshot("b").await,
        generator,
    );
    assert_eq!(again, Vec::new(), "after the script:\n{script}");
}
