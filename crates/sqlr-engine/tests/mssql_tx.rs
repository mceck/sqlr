//! Nested SQL Server transactions stay usable until their outermost commit.

mod support;

use std::time::Duration;

use sqlr_core::{Secrets, Value};
use sqlr_engine::{DriverRegistry, Engine, QueryStatus, SessionKey, SessionStatus, TxPhase};
use support::{config, dev, no_repaint, wait_until};

const TIMEOUT: Duration = Duration::from_secs(30);

fn run(engine: &Engine, key: &SessionKey, sql: &str) -> Vec<Vec<Value>> {
    let q = engine.execute_in(key, None, sql.into(), 100);
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(|r| { r.read().status != QueryStatus::Running })));
    let result = engine.result(q).unwrap();
    let result = result.read();
    assert!(
        matches!(result.status, QueryStatus::Ready { .. }),
        "{sql}: {:?}",
        result.status
    );
    (0..result.rows.row_count())
        .map(|r| {
            (0..result.rows.columns().len())
                .map(|c| Value::from_cell(result.rows.cell(r, c).unwrap()))
                .collect()
        })
        .collect()
}

fn engine() -> Engine {
    let (host, port) = support::endpoint("MSSQL");
    let mut drivers = DriverRegistry::default();
    drivers.register(sqlr_driver_mssql::SqlServerDriver::new());
    let engine = Engine::new(drivers, no_repaint());
    for id in ["writer", "observer"] {
        let cfg = sqlr_core::ConnectionConfig {
            values: [
                ("host".into(), host.clone()),
                ("port".into(), port.to_string()),
                ("database".into(), "master".into()),
                ("user".into(), "sa".into()),
                ("encryption".into(), "not_supported".into()),
                ("trust_server_certificate".into(), "true".into()),
            ]
            .into(),
            ..config(id, "sqlserver")
        };
        engine.connect(
            cfg,
            Secrets([("password".into(), "yourStrong(!)Password".into())].into()),
            dev(),
        );
        assert!(wait_until(TIMEOUT, || matches!(
            engine.session_status(id),
            Some(SessionStatus::Connected(_))
        )));
    }
    engine
}

#[test]
fn nested_commits_keep_the_outer_transaction_available_to_the_toolbar() {
    sqlr_testkit::skip_container_tests!();
    let _lock = sqlr_testkit::TestLock::acquire("mssql");
    let engine = engine();
    let key = SessionKey::meta("writer");
    let observer = SessionKey::meta("observer");
    let name = sqlr_testkit::unique_name("nested_tx");
    run(
        &engine,
        &key,
        &format!("CREATE TABLE dbo.[{name}] (id INT)"),
    );
    run(&engine, &key, "BEGIN TRANSACTION");
    engine.savepoint(&key, "outer_sp".into());
    run(&engine, &key, "BEGIN TRANSACTION");
    run(&engine, &key, "BEGIN TRANSACTION");
    run(
        &engine,
        &key,
        &format!("INSERT INTO dbo.[{name}] VALUES (1)"),
    );
    run(&engine, &key, "COMMIT");
    assert_eq!(
        run(&engine, &key, "SELECT @@TRANCOUNT"),
        vec![vec![Value::Int(2)]]
    );
    let tx = engine.transaction(&key).unwrap();
    assert_eq!(
        tx.phase,
        TxPhase::Open,
        "an inner COMMIT must keep the toolbar enabled"
    );
    assert!(tx.wrote);
    assert_eq!(tx.savepoints, ["outer_sp"]);

    engine.commit(&key);
    assert_eq!(
        run(&engine, &key, "SELECT @@TRANCOUNT"),
        vec![vec![Value::Int(1)]]
    );
    assert_eq!(engine.transaction(&key).unwrap().phase, TxPhase::Open);
    engine.commit(&key);
    assert_eq!(
        run(&engine, &key, "SELECT @@TRANCOUNT"),
        vec![vec![Value::Int(0)]]
    );
    assert_eq!(engine.transaction(&key).unwrap().phase, TxPhase::Idle);
    assert_eq!(
        run(&engine, &observer, &format!("SELECT id FROM dbo.[{name}]")),
        vec![vec![Value::Int(1)]]
    );
    run(&engine, &key, &format!("DROP TABLE dbo.[{name}]"));
}

#[test]
fn rollback_clears_every_nested_level_before_the_next_transaction() {
    sqlr_testkit::skip_container_tests!();
    let _lock = sqlr_testkit::TestLock::acquire("mssql");
    let engine = engine();
    let key = SessionKey::meta("writer");
    let observer = SessionKey::meta("observer");
    let name = sqlr_testkit::unique_name("nested_rollback");
    run(
        &engine,
        &key,
        &format!("CREATE TABLE dbo.[{name}] (id INT)"),
    );
    for toolbar in [false, true] {
        run(&engine, &key, "BEGIN TRANSACTION");
        run(&engine, &key, "BEGIN TRANSACTION");
        run(
            &engine,
            &key,
            &format!("INSERT INTO dbo.[{name}] VALUES (1)"),
        );
        if toolbar {
            engine.rollback(&key);
        } else {
            run(&engine, &key, "ROLLBACK");
        }
        assert_eq!(
            run(&engine, &key, "SELECT @@TRANCOUNT"),
            vec![vec![Value::Int(0)]]
        );
        assert_eq!(engine.transaction(&key).unwrap().phase, TxPhase::Idle);
        assert!(run(&engine, &observer, &format!("SELECT id FROM dbo.[{name}]")).is_empty());
        run(&engine, &key, "BEGIN TRANSACTION");
        run(
            &engine,
            &key,
            &format!("INSERT INTO dbo.[{name}] VALUES (2)"),
        );
        engine.commit(&key);
        assert_eq!(
            run(&engine, &key, "SELECT @@TRANCOUNT"),
            vec![vec![Value::Int(0)]]
        );
        assert_eq!(engine.transaction(&key).unwrap().phase, TxPhase::Idle);
        assert_eq!(
            run(&engine, &observer, &format!("SELECT id FROM dbo.[{name}]")),
            vec![vec![Value::Int(2)]]
        );
        run(&engine, &key, &format!("DELETE FROM dbo.[{name}]"));
    }
    run(&engine, &key, &format!("DROP TABLE dbo.[{name}]"));
}

#[test]
fn begin_end_blocks_do_not_add_a_transaction_level() {
    sqlr_testkit::skip_container_tests!();
    let _lock = sqlr_testkit::TestLock::acquire("mssql");
    let engine = engine();
    let key = SessionKey::meta("writer");
    let observer = SessionKey::meta("observer");
    let name = sqlr_testkit::unique_name("block_tx");
    run(
        &engine,
        &key,
        &format!("CREATE TABLE dbo.[{name}] (id INT)"),
    );
    run(
        &engine,
        &key,
        &format!("INSERT INTO dbo.[{name}] VALUES (1)"),
    );
    run(&engine, &key, "BEGIN TRANSACTION");
    run(
        &engine,
        &key,
        &format!("BEGIN UPDATE dbo.[{name}] SET id = 2; END"),
    );
    run(&engine, &key, "COMMIT");
    assert_eq!(
        run(&engine, &key, "SELECT @@TRANCOUNT"),
        vec![vec![Value::Int(0)]]
    );
    assert_eq!(engine.transaction(&key).unwrap().phase, TxPhase::Idle);
    assert_eq!(
        run(&engine, &observer, &format!("SELECT id FROM dbo.[{name}]")),
        vec![vec![Value::Int(2)]]
    );
    run(&engine, &key, &format!("DROP TABLE dbo.[{name}]"));
}
