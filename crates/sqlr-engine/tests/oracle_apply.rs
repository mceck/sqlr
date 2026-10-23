//! Default-only grid inserts through the engine and a real Oracle connection.

mod support;

use std::time::Duration;

use sqlr_core::{Change, Changeset, Secrets, TableRef, Value};
use sqlr_engine::{DriverRegistry, Engine, JobOutcome, QueryStatus, SessionKey, SessionStatus};
use support::{config, dev, no_repaint, wait_until};

const TIMEOUT: Duration = Duration::from_secs(30);

fn run(engine: &Engine, key: &SessionKey, sql: &str) -> Vec<Vec<Value>> {
    let q = engine.execute_in(key, None, sql.into(), 100);
    assert!(wait_until(TIMEOUT, || engine.result(q).is_some_and(|r| {
        r.read().status != QueryStatus::Running
    })));
    let result = engine.result(q).unwrap();
    let result = result.read();
    assert!(matches!(result.status, QueryStatus::Ready { .. }), "{sql}: {:?}", result.status);
    (0..result.rows.row_count())
        .map(|r| (0..result.rows.columns().len())
            .map(|c| Value::from_cell(result.rows.cell(r, c).unwrap()))
            .collect())
        .collect()
}

fn default_insert(definition: &str, keys: &[&str], projection: &str, expected: Vec<Vec<Value>>) {
    let _lock = sqlr_testkit::TestLock::acquire("oracle");
    let (host, port) = support::endpoint("ORACLE");
    let cfg = sqlr_core::ConnectionConfig {
        values: [("host".into(), host), ("port".into(), port.to_string()),
            ("service".into(), "FREEPDB1".into()), ("user".into(), "sqlr_test".into())].into(),
        ..config("o", "oracle")
    };
    let mut drivers = DriverRegistry::default();
    drivers.register(sqlr_driver_oracle::OracleDriver::new());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(cfg, Secrets([("password".into(), "sqlr_test".into())].into()), dev());
    assert!(wait_until(TIMEOUT, || matches!(engine.session_status("o"), Some(SessionStatus::Connected(_)))), "{:?}", engine.session_status("o"));
    let key = SessionKey::meta("o");
    let name = sqlr_testkit::unique_name("DEFAULT_ROW").to_uppercase();
    run(&engine, &key, &format!("CREATE TABLE \"{name}\" ({definition})"));
    let job = engine.apply_changeset(&key, Changeset {
        table: TableRef { database: None, schema: Some("SQLR_TEST".into()), name: name.clone() },
        key_columns: keys.iter().map(|s| s.to_string()).collect(),
        changes: vec![Change::Insert { values: Vec::new() }],
    });
    assert!(wait_until(TIMEOUT, || engine.job(job).is_some_and(|j| j.outcome.is_some())));
    let outcome = engine.job(job).unwrap().outcome.unwrap();
    let applied = matches!(&outcome, JobOutcome::Applied(r) if r.applied == 1 && r.failed.is_none());
    let rows = if applied { run(&engine, &key, &format!("SELECT {projection} FROM \"{name}\"")) } else { Vec::new() };
    run(&engine, &key, &format!("DROP TABLE \"{name}\" PURGE"));
    assert!(applied, "{outcome:?}");
    assert_eq!(rows, expected);
}

#[test]
fn default_insert_does_not_assign_to_a_virtual_primary_key() {
    sqlr_testkit::skip_container_tests!();
    default_insert(
        "base NUMBER DEFAULT 7, virtual_id GENERATED ALWAYS AS (base) VIRTUAL PRIMARY KEY",
        &["VIRTUAL_ID"],
        "base, virtual_id",
        vec![vec![Value::Int(7), Value::Int(7)]],
    );
}

#[test]
fn default_insert_into_an_identity_only_table_needs_no_row_key() {
    sqlr_testkit::skip_container_tests!();
    default_insert(
        "id NUMBER GENERATED ALWAYS AS IDENTITY",
        &[],
        "id",
        vec![vec![Value::Int(1)]],
    );
}
