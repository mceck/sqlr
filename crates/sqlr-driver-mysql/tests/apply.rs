//! Grid edits applied through the engine against MySQL 8.4 and MariaDB 11.4.

#[allow(dead_code)]
mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlr_core::{Change, Changeset, ConnectionConfig, ConnectionProfile, Secrets, TableRef, Value};
use sqlr_driver_mysql::MysqlDriver;
use sqlr_engine::{
    DriverRegistry, Engine, JobOutcome, LoadState, QueryId, QueryStatus, SessionKey, SessionStatus,
    Slot,
};
use support::{Server, start};

mod mysql84 {
    #[tokio::test(flavor = "multi_thread")]
    async fn apply_changeset_end_to_end() {
        if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
            return;
        }
        super::apply_changeset_end_to_end(super::Server::MySql).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_to_the_current_value_succeeds() {
        if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
            return;
        }
        super::update_to_the_current_value_succeeds(super::Server::MySql).await
    }
}

mod mariadb114 {
    #[tokio::test(flavor = "multi_thread")]
    async fn apply_changeset_end_to_end() {
        if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
            return;
        }
        super::apply_changeset_end_to_end(super::Server::MariaDb).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_to_the_current_value_succeeds() {
        if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
            return;
        }
        super::update_to_the_current_value_succeeds(super::Server::MariaDb).await
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
    drivers.register(MysqlDriver::new());
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

async fn apply_changeset_end_to_end(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let engine = engine(cfg, sec).await;
    run(
        &engine,
        "CREATE TABLE `My Items` (id INT AUTO_INCREMENT PRIMARY KEY, name TEXT NOT NULL, \
         qty INT DEFAULT 7, photo BLOB)",
    )
    .await;
    run(
        &engine,
        "INSERT INTO `My Items` (name, qty) VALUES ('a', 1), ('b', 2), ('c', 3)",
    )
    .await;
    let q = run(&engine, "SELECT * FROM `My Items` ORDER BY id").await;
    let table = origin_table(&engine, q);
    assert_eq!(
        table,
        TableRef {
            database: None,
            schema: Some("test".into()),
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
            "SELECT id, name, qty, photo FROM `My Items` ORDER BY id"
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
        rows(&engine, "SELECT name FROM `My Items` WHERE id = 2").await,
        vec![vec![Value::Text("b".into())]]
    );
}

/// MySQL counts changed rows by default: an UPDATE that sets a value to what it already is
/// would affect 0 rows and look like a concurrent change. The driver counts matched rows.
async fn update_to_the_current_value_succeeds(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let engine = engine(cfg, sec).await;
    run(&engine, "CREATE TABLE same (id INT PRIMARY KEY, v TEXT)").await;
    run(&engine, "INSERT INTO same VALUES (1, 'x')").await;
    let q = run(&engine, "SELECT * FROM same").await;
    let outcome = apply(
        &engine,
        Changeset {
            table: origin_table(&engine, q),
            key_columns: vec!["id".into()],
            changes: vec![Change::Update {
                key: vec![Value::Int(1)],
                column: "v".into(),
                value: Value::Text("x".into()),
            }],
        },
    )
    .await;
    assert!(
        matches!(&outcome, JobOutcome::Applied(r) if r.applied == 1 && r.failed.is_none()),
        "{outcome:?}"
    );
}
