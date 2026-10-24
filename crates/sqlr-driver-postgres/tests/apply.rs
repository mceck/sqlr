//! Grid edits applied through the engine against a real PostgreSQL server.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlr_core::{
    Change, Changeset, ConnectionConfig, ConnectionProfile, Driver, Execution, Secrets, TableRef,
    Value,
};
use sqlr_driver_postgres::PostgresDriver;
use sqlr_engine::{
    DriverRegistry, Engine, JobOutcome, LoadState, QueryId, QueryStatus, SessionKey, SessionStatus,
    Slot,
};
use support::pg;

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn engine(cfg: ConnectionConfig, sec: Secrets) -> Engine {
    let mut drivers = DriverRegistry::default();
    drivers.register(PostgresDriver::new());
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
async fn chained_transaction_stays_open_and_can_be_rolled_back_from_the_engine() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let (_server, cfg, sec) = pg().await;
    let engine = engine(cfg, sec).await;
    run(&engine, "CREATE TABLE chained (id int)").await;
    for ending in ["COMMIT AND CHAIN", "ROLLBACK AND CHAIN"] {
        run(&engine, "BEGIN").await;
        run(&engine, "SAVEPOINT before_chain").await;
        run(&engine, ending).await;
        let tx = engine.transaction(&tab()).unwrap();
        assert_eq!(tx.phase, sqlr_engine::TxPhase::Open, "{ending}");
        assert!(tx.savepoints.is_empty());
        assert_eq!(tx.statements, 0);
        run(&engine, "INSERT INTO chained VALUES (1)").await;
        assert!(engine.transaction(&tab()).unwrap().wrote);
        engine.rollback(&tab());
        until("rollback of the chained transaction", || {
            engine.transaction(&tab()).unwrap().phase == sqlr_engine::TxPhase::Idle
        })
        .await;
        assert!(rows(&engine, "SELECT * FROM chained").await.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_changeset_end_to_end() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let (_c, cfg, sec) = pg().await;
    let engine = engine(cfg, sec).await;
    run(
        &engine,
        "CREATE TABLE \"My Items\" (id serial PRIMARY KEY, name text NOT NULL, \
         qty int DEFAULT 7, photo bytea)",
    )
    .await;
    run(
        &engine,
        "INSERT INTO \"My Items\" (name, qty) VALUES ('a', 1), ('b', 2), ('c', 3)",
    )
    .await;
    let q = run(&engine, "SELECT * FROM \"My Items\" ORDER BY id").await;
    let table = origin_table(&engine, q);
    assert_eq!(table.name, "My Items");
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
            table,
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
                // `id` and `qty` take their defaults.
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
            table: origin_table(&engine, q),
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
async fn apply_refuses_a_table_renamed_by_another_session() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let (_c, cfg, sec) = pg().await;
    let other = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    let engine = engine(cfg, sec).await;
    run(&engine, "CREATE TABLE t (id int PRIMARY KEY, v text)").await;
    run(&engine, "INSERT INTO t VALUES (1, 'old')").await;
    // The session caches t's oid as "public.t".
    run(&engine, "SELECT * FROM t").await;
    for sql in [
        "ALTER TABLE t RENAME TO t_old",
        "CREATE TABLE t (id int PRIMARY KEY, v text)",
        "INSERT INTO t VALUES (1, 'new')",
    ] {
        match other.relational().unwrap().execute(sql).await.unwrap() {
            Execution::Affected(_) => {}
            Execution::Rows(mut s) => s.close().await.unwrap(),
        }
    }
    // Reading the renamed table reuses the stale cached name.
    let q = run(&engine, "SELECT * FROM t_old").await;
    let table = origin_table(&engine, q);
    let outcome = apply(
        &engine,
        Changeset {
            table,
            key_columns: vec!["id".into()],
            changes: vec![Change::Update {
                key: vec![Value::Int(1)],
                column: "v".into(),
                value: Value::Text("edited".into()),
            }],
        },
    )
    .await;
    let JobOutcome::Failed(e) = outcome else {
        panic!("the stale origin must be refused: {outcome:?}");
    };
    assert!(e.message.contains("renamed or dropped"), "{}", e.message);
    assert_eq!(
        rows(&engine, "SELECT v FROM t UNION ALL SELECT v FROM t_old").await,
        vec![
            vec![Value::Text("new".into())],
            vec![Value::Text("old".into())]
        ]
    );
    // The next read resolves the origin afresh.
    let q = run(&engine, "SELECT * FROM t_old").await;
    assert_eq!(origin_table(&engine, q).name, "t_old");
}
