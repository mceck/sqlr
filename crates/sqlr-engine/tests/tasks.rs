//! Saved tasks: running them, their run history and validation (spec §8.5).

mod support;

use std::path::{Path, PathBuf};

use sqlr_core::{
    ConnectionConfig, CsvOptions, Endpoint, SavedTask, Secrets, TableRef, TaskRun, TaskSpec,
    TextFormat, TransferOptions, TransferSpec,
};
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

fn run_sql(engine: &Engine, conn: &str, sql: &str) {
    let q = engine.execute(conn, sql.into(), 100);
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
}

fn count(engine: &Engine, conn: &str, table: &str) -> i64 {
    let q = engine.execute(conn, format!("SELECT count(*) FROM {table}"), 100);
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
    let result = engine.result(q).unwrap();
    let r = result.read();
    match sqlr_core::Value::from_cell(r.rows.cell(0, 0).unwrap()) {
        sqlr_core::Value::Int(n) => n,
        other => panic!("{other:?}"),
    }
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

fn table(name: &str) -> TableRef {
    TableRef {
        database: None,
        schema: Some("main".into()),
        name: name.into(),
    }
}

fn task(id: &str, spec: TransferSpec) -> SavedTask {
    SavedTask {
        id: id.into(),
        name: format!("task {id}"),
        spec: TaskSpec::Transfer(spec),
        created_ms: 1,
        updated_ms: 1,
    }
}

fn spec(source: Endpoint, target: Endpoint) -> TransferSpec {
    TransferSpec {
        source,
        target,
        mapping: Vec::new(),
        options: TransferOptions::default(),
    }
}

fn runs(engine: &Engine, id: &str) -> Vec<TaskRun> {
    let job = engine.task_runs(id);
    match finished(engine, job) {
        JobOutcome::TaskRuns(runs) => runs,
        other => panic!("{other:?}"),
    }
}

/// `a` holds `src` with three rows; `b` is empty.
fn two_dbs(dir: &Path) -> Engine {
    let engine = engine_with(&[("a", &dir.join("a.db")), ("b", &dir.join("b.db"))]);
    run_sql(&engine, "a", "CREATE TABLE src (id INTEGER, name TEXT)");
    run_sql(
        &engine,
        "a",
        "INSERT INTO src VALUES (1, 'x'), (2, 'y'), (3, 'z')",
    );
    engine
}

fn copy_task(id: &str) -> SavedTask {
    task(
        id,
        spec(
            Endpoint::Table {
                conn: "a".into(),
                table: table("src"),
            },
            Endpoint::NewTable {
                conn: "b".into(),
                table: table("copy"),
                ddl: None,
            },
        ),
    )
}

#[test]
fn run_task_records_run() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    engine.save_task(copy_task("t1"));
    assert!(wait_until(TIMEOUT, || engine.tasks().len() == 1));

    let job = engine.run_task("t1").unwrap();
    assert!(matches!(
        finished(&engine, job),
        JobOutcome::Imported { written: 3, .. }
    ));
    assert_eq!(count(&engine, "b", "copy"), 3);

    // The run is on record, with its end, outcome and summary.
    assert!(wait_until(TIMEOUT, || {
        runs(&engine, "t1")
            .first()
            .is_some_and(|r| r.ended_ms.is_some())
    }));
    let history = runs(&engine, "t1");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].outcome, "ok");
    assert!(
        history[0].summary.contains("wrote 3"),
        "{}",
        history[0].summary
    );

    // Running again adds a run; deleting the task drops them.
    run_sql(&engine, "b", "DROP TABLE copy");
    let job = engine.run_task("t1").unwrap();
    finished(&engine, job);
    assert!(wait_until(TIMEOUT, || runs(&engine, "t1").len() == 2));
    engine.delete_task("t1");
    assert!(wait_until(TIMEOUT, || engine.tasks().is_empty()));
    assert!(wait_until(TIMEOUT, || runs(&engine, "t1").is_empty()));
}

#[test]
fn task_with_missing_connection() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    let mut broken = copy_task("t1");
    broken.spec = TaskSpec::Transfer(spec(
        Endpoint::Table {
            conn: "gone".into(),
            table: table("src"),
        },
        Endpoint::NewTable {
            conn: "b".into(),
            table: table("copy"),
            ddl: None,
        },
    ));
    engine.save_task(broken);
    assert!(wait_until(TIMEOUT, || engine.tasks().len() == 1));
    let err = engine.run_task("t1").unwrap_err();
    assert!(
        err.contains("gone"),
        "the message names the connection: {err}"
    );
    assert!(
        runs(&engine, "t1").is_empty(),
        "a task that fails validation has no run"
    );

    // A missing input file is named too.
    let mut file = copy_task("t2");
    file.spec = TaskSpec::Transfer(spec(
        Endpoint::File {
            path: dir.path().join("nope.csv"),
            format: TextFormat::Csv(CsvOptions::default()),
        },
        Endpoint::NewTable {
            conn: "b".into(),
            table: table("copy"),
            ddl: None,
        },
    ));
    engine.save_task(file);
    assert!(wait_until(TIMEOUT, || engine.tasks().len() == 2));
    let err = engine.run_task("t2").unwrap_err();
    assert!(err.contains("nope.csv"), "{err}");

    assert!(engine.run_task("unknown").is_err());
}

#[test]
fn cancel_task_run() {
    let dir = tempfile::tempdir().unwrap();
    let engine = two_dbs(dir.path());
    let big: PathBuf = dir.path().join("big.csv");
    let mut text = String::from("id,name\n");
    for i in 0..400_000 {
        text.push_str(&format!("{i},n{i}\n"));
    }
    std::fs::write(&big, text).unwrap();
    run_sql(&engine, "b", "CREATE TABLE dst (id INTEGER, name TEXT)");
    let mut s = spec(
        Endpoint::File {
            path: big,
            format: TextFormat::Csv(CsvOptions::default()),
        },
        Endpoint::Table {
            conn: "b".into(),
            table: table("dst"),
        },
    );
    s.options.batch = 100;
    engine.save_task(task("t1", s));
    assert!(wait_until(TIMEOUT, || engine.tasks().len() == 1));
    let job = engine.run_task("t1").unwrap();
    assert!(wait_until(TIMEOUT * 6, || engine.job(job).is_some_and(
        |j| j.progress.rows >= 2_000 || j.outcome.is_some()
    )));
    engine.cancel_job(job);
    assert_eq!(finished(&engine, job), JobOutcome::Cancelled);
    assert!(wait_until(TIMEOUT, || {
        runs(&engine, "t1")
            .first()
            .is_some_and(|r| r.outcome == "cancelled")
    }));
}
