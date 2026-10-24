//! Schema compare jobs (spec §8.6), on two SQLite files.

use crate::support;

use std::collections::HashSet;
use std::path::Path;

use sqlr_core::{ConnectionConfig, DiffKind, ObjectRef, Secrets};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_engine::{DriverRegistry, Engine, JobId, JobOutcome, QueryStatus, SessionStatus};
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn engine_with(dbs: &[(&str, &Path)]) -> Engine {
    let mut drivers = DriverRegistry::default();
    drivers.register(SqliteDriver::new());
    let engine = Engine::new(drivers, no_repaint());
    for (id, path) in dbs {
        let cfg = ConnectionConfig {
            values: [("path".to_string(), path.display().to_string())].into(),
            ..config(id, "sqlite")
        };
        engine.connect(cfg, Secrets::default(), dev());
        assert!(wait_until(TIMEOUT, || matches!(
            engine.session_status(id),
            Some(SessionStatus::Connected(_))
        )));
    }
    engine
}

fn run(engine: &Engine, conn: &str, sql: &str) {
    let q = engine.execute(conn, sql.into(), 100);
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
    let status = engine.result(q).unwrap().read().status.clone();
    assert!(
        matches!(status, QueryStatus::Ready { .. }),
        "{sql}: {status:?}"
    );
}

fn finished(engine: &Engine, job: JobId) -> JobOutcome {
    assert!(
        wait_until(TIMEOUT * 6, || engine
            .job(job)
            .is_some_and(|j| j.outcome.is_some())),
        "the job never finished"
    );
    engine.job(job).unwrap().outcome.unwrap()
}

fn schema(conn: &str) -> (String, Option<String>, String) {
    (conn.into(), None, "main".into())
}

#[test]
fn compare_schemas_reports_differences_and_scripts_them() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_with(&[
        ("a", &dir.path().join("a.db")),
        ("b", &dir.path().join("b.db")),
    ]);
    run(
        &engine,
        "a",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL)",
    );
    run(&engine, "a", "CREATE TABLE fresh (id INTEGER PRIMARY KEY)");
    run(
        &engine,
        "b",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT)",
    );
    run(&engine, "b", "CREATE TABLE legacy (id INTEGER PRIMARY KEY)");

    let job = engine.compare_schemas(schema("a"), schema("b"));
    let JobOutcome::Compared(diffs) = finished(&engine, job) else {
        panic!("a compare outcome expected");
    };
    let got: Vec<(String, DiffKind)> = diffs
        .iter()
        .map(|d| (d.object.name.clone(), d.kind))
        .collect();
    assert_eq!(
        got,
        [
            ("fresh".to_string(), DiffKind::Added),
            ("legacy".to_string(), DiffKind::Removed),
            ("users".to_string(), DiffKind::Changed),
        ]
    );

    // The script is for the target; excluding an object leaves it out.
    let script = engine
        .migration_script("b", &diffs, &HashSet::new(), "-- migrate b")
        .unwrap();
    assert!(script.starts_with("-- migrate b"), "{script}");
    assert!(script.contains("fresh"), "{script}");
    assert!(script.contains("legacy"), "{script}");
    let legacy: ObjectRef = diffs
        .iter()
        .find(|d| d.object.name == "legacy")
        .unwrap()
        .object
        .clone();
    let script = engine
        .migration_script("b", &diffs, &HashSet::from([legacy]), "")
        .unwrap();
    assert!(!script.contains("legacy"), "{script}");
}

#[test]
fn compare_refuses_other_engines_and_unconnected_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_with(&[("a", &dir.path().join("a.db"))]);
    let job = engine.compare_schemas(schema("a"), schema("missing"));
    assert!(matches!(finished(&engine, job), JobOutcome::Failed(_)));
}

#[test]
fn explain_job_returns_a_plan() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_with(&[("a", &dir.path().join("a.db"))]);
    run(
        &engine,
        "a",
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT)",
    );
    let key = sqlr_engine::SessionKey::meta("a");
    let job = engine.explain(&key, "SELECT * FROM t WHERE a = 'x'".into(), false);
    match finished(&engine, job) {
        JobOutcome::Plan(plan) => assert!(!plan.root.children.is_empty(), "{plan:?}"),
        other => panic!("{other:?}"),
    }
    // A tab session that has not run anything yet opens on demand, like a query.
    let tab = engine.session("a", Some(7));
    assert!(tab.slot != sqlr_engine::Slot::Meta);
    let job = engine.explain(&tab, "SELECT * FROM t".into(), false);
    match finished(&engine, job) {
        JobOutcome::Plan(plan) => assert!(!plan.root.children.is_empty(), "{plan:?}"),
        other => panic!("{other:?}"),
    }
    // A DDL statement gets a message, not an error dump.
    let job = engine.explain(&key, "CREATE TABLE u (a int)".into(), false);
    let JobOutcome::Failed(e) = finished(&engine, job) else {
        panic!("a failure");
    };
    assert!(e.message.starts_with("Explain"), "{}", e.message);
    let job = engine.explain(
        &sqlr_engine::SessionKey::meta("zzz"),
        "SELECT 1".into(),
        false,
    );
    assert!(matches!(finished(&engine, job), JobOutcome::Failed(_)));
}

#[test]
fn er_model_job_and_layout_storage() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_with(&[("a", &dir.path().join("a.db"))]);
    run(&engine, "a", "CREATE TABLE users (id INTEGER PRIMARY KEY)");
    run(
        &engine,
        "a",
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id))",
    );
    run(&engine, "a", "CREATE TABLE other (id INTEGER)");
    let job = engine.er_model("a", None, "main".into(), None);
    let JobOutcome::ErModel(model) = finished(&engine, job) else {
        panic!("a model");
    };
    assert_eq!(model.tables.len(), 3);
    assert_eq!(model.edges.len(), 1);
    // A selection keeps only its tables.
    let job = engine.er_model(
        "a",
        None,
        "main".into(),
        Some(vec!["users".into(), "orders".into()]),
    );
    let JobOutcome::ErModel(model) = finished(&engine, job) else {
        panic!("a model");
    };
    assert_eq!(model.tables.len(), 2);

    // Layouts are stored under a key and read back.
    let job = engine.load_er_layout("k1");
    assert_eq!(finished(&engine, job), JobOutcome::ErLayout(None));
    engine.save_er_layout("k1", "[[1,2]]");
    let job = engine.load_er_layout("k1");
    assert_eq!(
        finished(&engine, job),
        JobOutcome::ErLayout(Some("[[1,2]]".into()))
    );
}
