use crate::support;

use sqlr_core::{CommitMode, ConnectionProfile, Secrets};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_engine::{DriverRegistry, Engine, QueryStatus, SessionStatus};
use support::{TIMEOUT, wait_until};

fn engine(dir: &std::path::Path) -> Engine {
    let mut drivers = DriverRegistry::default();
    drivers.register(SqliteDriver::new());
    let engine = Engine::new(drivers, support::no_repaint());
    let mut cfg = support::config("c", "sqlite");
    cfg.values
        .insert("path".into(), dir.join("agent.db").display().to_string());
    cfg.values.insert("create_if_missing".into(), "true".into());
    let profile = ConnectionProfile {
        commit_mode: CommitMode::Auto,
        ..ConnectionProfile::development()
    };
    engine.connect(cfg, Secrets::default(), profile);
    assert!(
        wait_until(TIMEOUT, || matches!(
            engine.session_status("c"),
            Some(SessionStatus::Connected(_))
        )),
        "connect"
    );
    engine
}

fn wait_result(engine: &Engine, q: sqlr_engine::QueryId) -> QueryStatus {
    assert!(
        wait_until(TIMEOUT, || {
            engine
                .result(q)
                .is_some_and(|r| r.read().status != QueryStatus::Running)
        }),
        "query finished"
    );
    engine.result(q).unwrap().read().status.clone()
}

#[test]
fn agent_sessions_are_isolated_and_closing_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let created = engine.execute_on("c", None, "create table t (id integer)".into(), 10);
    wait_result(&engine, created);

    let first = engine.agent_session("c", "session-one");
    let second = engine.agent_session("c", "session-two");
    engine.prepare_agent_session(&first, false);
    engine.prepare_agent_session(&second, false);
    engine.set_commit_mode(&first, CommitMode::Manual);
    engine.set_commit_mode(&second, CommitMode::Manual);

    // Each MCP session has its own engine session and transaction.
    assert_ne!(first, second);
    let one = engine.execute_in(&first, None, "insert into t values (1)".into(), 10);
    wait_result(&engine, one);
    assert!(
        engine
            .open_transactions()
            .iter()
            .any(|(key, _)| key == &first),
        "the first agent has its own transaction"
    );

    // Ending the first MCP session rolls its transaction back; the second commits.
    engine.close_agent_sessions("session-one");
    let two = engine.execute_in(&second, None, "insert into t values (2)".into(), 10);
    wait_result(&engine, two);
    engine.commit(&second);
    assert!(
        wait_until(TIMEOUT, || {
            engine
                .transaction(&second)
                .is_some_and(|tx| tx.phase == sqlr_engine::TxPhase::Idle)
        }),
        "commit"
    );
    let q = engine.execute_on("c", None, "select count(*) from t".into(), 10);
    assert_eq!(wait_result(&engine, q), QueryStatus::Ready { more: false });
    let result = engine.result(q).unwrap();
    let result = result.read();
    assert_eq!(result.rows.cell(0, 0), Some(sqlr_core::Cell::Int(1)));
    drop(result);
    engine.close_result(q);
    engine.close_agent_sessions("session-two");
}

#[test]
fn a_read_exposure_agent_session_refuses_writes() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let created = engine.execute_on("c", None, "create table t (id integer)".into(), 10);
    wait_result(&engine, created);
    let key = engine.agent_session("c", "read-only");
    engine.prepare_agent_session(&key, true);
    let q = engine.execute_in(&key, None, "insert into t values (1)".into(), 10);
    let status = wait_result(&engine, q);
    assert!(
        matches!(status, QueryStatus::Failed(_)),
        "a read-only SQLite session refuses writes: {status:?}"
    );
    engine.close_result(q);
    engine.close_agent_sessions("read-only");
}

#[test]
fn the_metadata_session_stays_open_when_an_agent_goes_away() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let key = engine.agent_session("c", "temporary");
    engine.prepare_agent_session(&key, false);
    let q = engine.execute_in(&key, None, "select 1".into(), 10);
    wait_result(&engine, q);
    engine.close_agent_sessions("temporary");
    assert!(matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    ));
}
