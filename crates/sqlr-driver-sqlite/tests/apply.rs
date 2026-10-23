//! Grid edits applied through the engine against a real SQLite database.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlr_core::{Change, Changeset, ConnectionConfig, ConnectionProfile, Secrets, TableRef, Value};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_engine::{
    DriverRegistry, Engine, JobOutcome, LoadState, QueryId, QueryStatus, SessionKey, SessionStatus,
    Slot,
};

fn config(path: &Path) -> ConnectionConfig {
    ConnectionConfig {
        id: "t".into(),
        name: "test".into(),
        driver: "sqlite".into(),
        group: None,
        color: None,
        values: [("path".to_string(), path.display().to_string())].into(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    }
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn engine(cfg: ConnectionConfig, sec: Secrets) -> Engine {
    let mut drivers = DriverRegistry::default();
    drivers.register(SqliteDriver::new());
    let engine = Engine::new(drivers, Arc::new(|| {}));
    engine.connect(cfg, sec, ConnectionProfile::development());
    until("the connection", || {
        matches!(
            engine.session_status("t"),
            Some(SessionStatus::Connected(_))
        )
    })
    .await;
    engine
}

fn tab() -> SessionKey {
    SessionKey {
        conn: "t".into(),
        slot: Slot::Tab(1),
    }
}

/// Runs `sql` on the tab's session to its end; panics on failure.
async fn run(engine: &Engine, sql: &str) -> QueryId {
    let q = engine.execute_in(&tab(), None, sql.into(), 1000);
    until(sql, || {
        engine
            .result(q)
            .is_some_and(|r| r.read().status != QueryStatus::Running)
    })
    .await;
    let status = engine.result(q).unwrap().read().status.clone();
    assert!(
        matches!(status, QueryStatus::Ready { .. }),
        "{sql}: {status:?}"
    );
    q
}

async fn rows(engine: &Engine, sql: &str) -> Vec<Vec<Value>> {
    let q = run(engine, sql).await;
    let result = engine.result(q).unwrap();
    let r = result.read();
    (0..r.rows.row_count())
        .map(|row| {
            (0..r.rows.columns().len())
                .map(|col| Value::from_cell(r.rows.cell(row, col).unwrap()))
                .collect()
        })
        .collect()
}

async fn apply(engine: &Engine, changeset: Changeset) -> JobOutcome {
    let job = engine.apply_changeset(&tab(), changeset);
    until("the changeset", || {
        engine.job(job).is_some_and(|j| j.outcome.is_some())
    })
    .await;
    engine.job(job).unwrap().outcome.unwrap()
}

/// The table the result's first column was read from.
fn origin_table(engine: &Engine, q: QueryId) -> TableRef {
    let result = engine.result(q).unwrap();
    let r = result.read();
    r.rows.columns()[0]
        .origin
        .clone()
        .expect("a table column has an origin")
        .table
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_changeset_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(config(&dir.path().join("t.db")), Secrets::default()).await;
    run(
        &engine,
        "CREATE TABLE \"My Items\" (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
         qty INTEGER DEFAULT 7, photo BLOB)",
    )
    .await;
    run(
        &engine,
        "INSERT INTO \"My Items\" (name, qty) VALUES ('a', 1), ('b', 2), ('c', 3)",
    )
    .await;
    let q = run(&engine, "SELECT * FROM \"My Items\" ORDER BY id").await;
    let table = origin_table(&engine, q);
    assert_eq!(
        table,
        TableRef {
            database: None,
            schema: Some("main".into()),
            name: "My Items".into()
        }
    );
    engine.describe(&tab(), table.clone());
    until("the table description", || {
        matches!(engine.table_info("t", &table), Some(LoadState::Ready(_)))
    })
    .await;
    let Some(LoadState::Ready(info)) = engine.table_info("t", &table) else {
        unreachable!()
    };
    assert_eq!(info.row_key(), Some(&["id".to_string()][..]));

    let outcome = apply(
        &engine,
        Changeset {
            table: table.clone(),
            key_columns: vec!["id".into()],
            changes: vec![
                Change::Update {
                    key: vec![Value::Int(1)],
                    column: "name".into(),
                    value: Value::Text("O'Brien".into()),
                },
                Change::Update {
                    key: vec![Value::Int(1)],
                    column: "photo".into(),
                    value: Value::Bytes(vec![0, 1, 255]),
                },
                Change::Insert {
                    values: vec![("name".into(), Value::Text("d".into()))],
                },
                Change::Delete {
                    key: vec![Value::Int(3)],
                },
            ],
        },
    )
    .await;
    assert!(
        matches!(&outcome, JobOutcome::Applied(r) if r.applied == 3 && r.failed.is_none()),
        "{outcome:?}"
    );
    assert_eq!(
        rows(
            &engine,
            "SELECT id, name, qty, photo FROM \"My Items\" ORDER BY id"
        )
        .await,
        vec![
            vec![
                Value::Int(1),
                Value::Text("O'Brien".into()),
                Value::Int(1),
                Value::Bytes(vec![0, 1, 255])
            ],
            vec![
                Value::Int(2),
                Value::Text("b".into()),
                Value::Int(2),
                Value::Null
            ],
            vec![
                Value::Int(4),
                Value::Text("d".into()),
                Value::Int(7),
                Value::Null
            ],
        ]
    );

    // A row deleted meanwhile: nothing of the changeset stays.
    let outcome = apply(
        &engine,
        Changeset {
            table,
            key_columns: vec!["id".into()],
            changes: vec![
                Change::Update {
                    key: vec![Value::Int(2)],
                    column: "name".into(),
                    value: Value::Text("changed".into()),
                },
                Change::Delete {
                    key: vec![Value::Int(3)],
                },
            ],
        },
    )
    .await;
    let JobOutcome::Applied(report) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(report.failed.unwrap().0, "row id=3");
    assert_eq!(
        rows(&engine, "SELECT name FROM \"My Items\" WHERE id = 2").await,
        vec![vec![Value::Text("b".into())]]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ddl_commit_failure_rolls_back_before_reusing_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(config(&dir.path().join("t.db")), Secrets::default()).await;
    for sql in [
        "PRAGMA foreign_keys = ON",
        "CREATE TABLE parent (id INTEGER PRIMARY KEY)",
        "CREATE TABLE child (parent_id INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED)",
        "INSERT INTO parent VALUES (1)",
        "INSERT INTO child VALUES (1)",
    ] {
        run(&engine, sql).await;
    }
    // DROP succeeds, but the deferred foreign key rejects COMMIT. The failed apply
    // must undo the DROP and release the transaction before returning its report.
    let job = engine.apply_ddl(
        &tab(),
        vec!["DROP TABLE parent".into()],
        sqlr_engine::ApplyOptions {
            transactional: true,
            ..Default::default()
        },
    );
    until("the DDL apply", || {
        engine.job(job).is_some_and(|j| j.outcome.is_some())
    })
    .await;
    let outcome = engine.job(job).unwrap().outcome.unwrap();
    let JobOutcome::DdlApplied(report) = outcome else {
        panic!("{outcome:?}");
    };
    assert!(!report.ok());
    assert_eq!(report.control[0].0, "COMMIT");
    assert!(report.rolled_back);
    run(&engine, "BEGIN").await;
    assert_eq!(
        rows(&engine, "SELECT id FROM parent").await,
        vec![vec![Value::Int(1)]]
    );
    assert_eq!(
        rows(&engine, "SELECT parent_id FROM child").await,
        vec![vec![Value::Int(1)]]
    );
    run(&engine, "ROLLBACK").await;
}
