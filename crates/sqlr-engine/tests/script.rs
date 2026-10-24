use std::sync::Arc;
use std::time::Duration;

use sqlr_core::{HistoryQuery, Secrets};
use sqlr_engine::{
    DriverRegistry, Engine, OutputLine, QueryId, QueryStatus, ScriptId, ScriptOnError, ScriptState,
    SessionStatus,
};
use sqlr_store::Db;
use sqlr_testkit::{FakeDriver, FakeScript};

use crate::support;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

/// An engine on `db` with one fake driver and the session "c" already `Connected`.
fn connected_on(script: FakeScript, db: Arc<Db>) -> (Engine, Arc<FakeDriver>) {
    let fake = FakeDriver::new(script);
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::with_db(drivers, no_repaint(), db);
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(
        wait_until(TIMEOUT, || matches!(
            engine.session_status("c"),
            Some(SessionStatus::Connected(_))
        )),
        "session never connected: {:?}",
        engine.session_status("c")
    );
    (engine, fake)
}

fn connected(script: FakeScript) -> (Engine, Arc<FakeDriver>) {
    connected_on(script, Arc::new(Db::open_in_memory().unwrap()))
}

fn statements(sqls: &[&str]) -> Vec<String> {
    sqls.iter().map(|s| s.to_string()).collect()
}

fn state(engine: &Engine, id: ScriptId) -> ScriptState {
    engine.script(id).expect("script exists").read().clone()
}

/// Waits until the script is `done`, then returns its state.
fn finished(engine: &Engine, id: ScriptId) -> ScriptState {
    assert!(
        wait_until(TIMEOUT, || state(engine, id).done),
        "script still running: {:?}",
        state(engine, id)
    );
    state(engine, id)
}

fn texts(output: &[OutputLine]) -> Vec<&str> {
    output.iter().map(|l| l.text.as_str()).collect()
}

/// `"Statement {k}: {what} · {n} ms"`.
fn assert_line(line: &OutputLine, statement: usize, what: &str) {
    let prefix = format!("Statement {}: {what} · ", statement + 1);
    let ms = line
        .text
        .strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix(" ms"))
        .unwrap_or_else(|| panic!("unexpected line {:?}, wanted {prefix:?}", line.text));
    assert!(ms.parse::<u64>().is_ok(), "line {:?}", line.text);
    assert_eq!(line.statement, statement);
    assert_eq!(line.error, None);
}

#[test]
fn runs_in_order_with_results_and_output() {
    let (engine, fake) = connected(FakeScript {
        rows: 10,
        ..Default::default()
    });
    let id = engine.execute_script(
        "c",
        statements(&["select 1", "select 2"]),
        500,
        ScriptOnError::Stop,
    );
    let s = finished(&engine, id);
    assert_eq!(fake.executed(), ["select 1", "select 2"]);
    assert_eq!(s.results.len(), 2);
    assert_eq!(s.results[0].0, 0);
    assert_eq!(s.results[1].0, 1);
    assert_eq!(s.output.len(), 2);
    assert_line(&s.output[0], 0, "10 rows");
    assert_line(&s.output[1], 1, "10 rows");
    assert_eq!(s.running, None);
    assert!(!s.cancelled);
    let first = engine.result(s.results[0].1).unwrap();
    assert_eq!(first.read().sql, "select 1");
}

#[test]
fn earlier_rows_result_is_truncated() {
    let (engine, _fake) = connected(FakeScript {
        rows: 1200,
        ..Default::default()
    });
    let id = engine.execute_script(
        "c",
        statements(&["select a", "select b"]),
        500,
        ScriptOnError::Stop,
    );
    let s = finished(&engine, id);
    assert_line(&s.output[0], 0, "500 rows");
    let first = engine.result(s.results[0].1).unwrap();
    let second = engine.result(s.results[1].1).unwrap();
    // The second statement took the session's one stream over from the first.
    assert!(wait_until(TIMEOUT, || first.read().status
        == QueryStatus::Ready { more: false }));
    assert!(first.read().cut_short);
    assert_eq!(first.read().rows.row_count(), 500);
    assert_eq!(second.read().status, QueryStatus::Ready { more: true });
    assert!(!second.read().cut_short);
}

#[test]
fn stop_on_error() {
    let (engine, fake) = connected(FakeScript {
        rows: 1,
        fail_matching: Some("bad".into()),
        ..Default::default()
    });
    let id = engine.execute_script(
        "c",
        statements(&["select 1", "bad", "select 3"]),
        500,
        ScriptOnError::Stop,
    );
    let s = finished(&engine, id);
    assert_eq!(fake.executed(), ["select 1", "bad"]);
    assert_eq!(s.output.len(), 3);
    let failed = &s.output[1];
    assert_eq!(failed.statement, 1);
    let e = failed.error.as_ref().expect("an error line");
    assert_eq!(failed.text, format!("Statement 2: {}", e.message));
    assert_eq!(
        s.output.last().unwrap().text,
        "Script stopped after statement 2"
    );
    assert_eq!(s.results.len(), 1);
    assert!(!s.cancelled);
}

#[test]
fn continue_on_error() {
    let (engine, fake) = connected(FakeScript {
        rows: 1,
        fail_matching: Some("bad".into()),
        ..Default::default()
    });
    let id = engine.execute_script(
        "c",
        statements(&["select 1", "bad", "select 3"]),
        500,
        ScriptOnError::Continue,
    );
    let s = finished(&engine, id);
    assert_eq!(fake.executed(), ["select 1", "bad", "select 3"]);
    assert_eq!(s.output.len(), 3);
    assert!(s.output[1].error.is_some());
    assert_line(&s.output[2], 2, "1 rows");
    let results: Vec<usize> = s.results.iter().map(|(i, _)| *i).collect();
    assert_eq!(results, [0, 2]);
}

#[test]
fn affected_output_line() {
    let (engine, _fake) = connected(FakeScript {
        affected: Some(7),
        ..Default::default()
    });
    let id = engine.execute_script(
        "c",
        statements(&["delete from t"]),
        500,
        ScriptOnError::Stop,
    );
    let s = finished(&engine, id);
    assert_line(&s.output[0], 0, "7 rows affected");
    assert!(s.results.is_empty());
}

#[test]
fn cancel_script_stops() {
    let (engine, fake) = connected(FakeScript {
        rows: 10,
        block_on_fetch: true,
        ..Default::default()
    });
    let id = engine.execute_script(
        "c",
        statements(&["select 1", "select 2", "select 3"]),
        500,
        ScriptOnError::Continue,
    );
    // The first statement is running once the driver has received it.
    assert!(wait_until(TIMEOUT, || fake.executed().len() == 1));
    assert_eq!(state(&engine, id).running, Some(0));
    engine.cancel_script(id);
    let s = finished(&engine, id);
    assert!(s.cancelled);
    assert_eq!(texts(&s.output), ["Script cancelled"]);
    assert_eq!(s.running, None);
    assert_eq!(fake.executed(), ["select 1"]);
}

#[test]
fn history_per_statement() {
    let db = Arc::new(Db::open_in_memory().unwrap());
    let (engine, _fake) = connected_on(
        FakeScript {
            rows: 1,
            ..Default::default()
        },
        db.clone(),
    );
    let id = engine.execute_script(
        "c",
        statements(&["select 1", "select 2"]),
        500,
        ScriptOnError::Stop,
    );
    finished(&engine, id);
    let stored = || db.search_history(&HistoryQuery::default()).unwrap();
    assert!(
        wait_until(TIMEOUT, || stored().len() == 2),
        "history: {:?}",
        stored()
    );
    let sqls: Vec<String> = stored().into_iter().map(|e| e.sql).collect();
    assert_eq!(sqls, ["select 2", "select 1"]);
}

#[test]
fn close_script_drops_it_and_its_results() {
    let (engine, _fake) = connected(FakeScript {
        rows: 1,
        ..Default::default()
    });
    let id = engine.execute_script(
        "c",
        statements(&["select 1", "select 2"]),
        500,
        ScriptOnError::Stop,
    );
    let s = finished(&engine, id);
    engine.close_script(id);
    assert!(engine.script(id).is_none());
    for (_, q) in s.results {
        assert!(engine.result(q).is_none());
    }
}

#[test]
fn not_connected_script_fails_and_stops() {
    let (engine, fake) = connected(FakeScript::default());
    let id = engine.execute_script(
        "other",
        statements(&["select 1", "select 2"]),
        500,
        ScriptOnError::Stop,
    );
    let s = finished(&engine, id);
    assert_eq!(
        texts(&s.output),
        [
            "Statement 1: not connected",
            "Script stopped after statement 1"
        ]
    );
    assert!(fake.executed().is_empty());
}

#[test]
fn script_runs_on_another_database() {
    let (engine, fake) = connected(FakeScript {
        rows: 1,
        databases: vec!["sales".into()],
        ..Default::default()
    });
    let id = engine.execute_script_on(
        "c",
        Some("sales"),
        statements(&["select 1", "select 2"]),
        500,
        ScriptOnError::Stop,
    );
    let s = finished(&engine, id);
    assert_eq!(s.results.len(), 2);
    assert_eq!(
        fake.executed_on(),
        [
            (Some("sales".to_string()), "select 1".to_string()),
            (Some("sales".to_string()), "select 2".to_string())
        ]
    );
}

#[test]
fn closing_the_running_result_cancels_the_script() {
    let (engine, fake) = connected(FakeScript {
        rows: 10,
        block_on_fetch: true,
        ..Default::default()
    });
    let id = engine.execute_script(
        "c",
        statements(&["select 1", "select 2"]),
        500,
        ScriptOnError::Continue,
    );
    assert!(wait_until(TIMEOUT, || fake.executed().len() == 1));
    // The engine issues query ids in order from 1, so the running statement's is 1.
    engine.close_result(QueryId(1));
    assert!(
        wait_until(Duration::from_secs(1), || {
            let s = state(&engine, id);
            s.done && s.cancelled
        }),
        "script: {:?}",
        state(&engine, id)
    );
    let s = state(&engine, id);
    assert_eq!(texts(&s.output), ["Script cancelled"]);
    assert_eq!(s.running, None);
    assert_eq!(fake.executed(), ["select 1"]);
}

#[test]
fn execute_after_script_queues_behind_its_first_statement() {
    let (engine, fake) = connected(FakeScript {
        rows: 1,
        ..Default::default()
    });
    let id = engine.execute_script(
        "c",
        statements(&["select 1", "select 2"]),
        500,
        ScriptOnError::Continue,
    );
    let q = engine.execute("c", "select x".into(), 500);
    finished(&engine, id);
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
    assert_eq!(fake.executed()[0], "select 1");
}
