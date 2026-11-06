//! A tab's session recovers from a server that stopped answering: a statement the server
//! never ends must not leave the tab unable to run anything else.

use std::sync::Arc;
use std::time::Duration;

use sqlr_core::{CommitMode, ConnectionProfile, ErrorKind, Secrets};
use sqlr_engine::{
    DriverRegistry, Engine, QueryId, QueryStatus, SessionKey, SessionStatus, TxPhase,
};
use sqlr_testkit::{FakeDriver, FakeScript};

use crate::support;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

/// The engine gives a connection up this long after an unanswered cancel.
const GRACE: Duration = Duration::from_millis(200);

/// An engine whose statements containing "hang" never get an answer, with tab 1's session.
fn hanging(profile: ConnectionProfile) -> (Engine, Arc<FakeDriver>, SessionKey) {
    let fake = FakeDriver::new(FakeScript {
        rows: 10,
        hang_matching: Some("hang".into()),
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.set_cancel_grace(GRACE);
    engine.connect(config("c", "fake"), Secrets::default(), profile);
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
    let key = engine.session("c", Some(1));
    (engine, fake, key)
}

fn status(engine: &Engine, q: QueryId) -> QueryStatus {
    engine.result(q).expect("result").read().status.clone()
}

fn settled(engine: &Engine, q: QueryId) -> QueryStatus {
    assert!(
        wait_until(TIMEOUT, || status(engine, q) != QueryStatus::Running),
        "query {q:?} still running"
    );
    status(engine, q)
}

fn messages(engine: &Engine, q: QueryId) -> Vec<String> {
    let result = engine.result(q).expect("result");
    let r = result.read();
    r.messages.iter().map(|m| m.text.clone()).collect()
}

/// Starts a statement the server never answers and waits until the driver has it.
fn start_hanging(engine: &Engine, fake: &FakeDriver, key: &SessionKey) -> QueryId {
    let before = fake.executed().len();
    let q = engine.execute_in(key, None, "select hang".into(), 100);
    assert!(
        wait_until(TIMEOUT, || fake.executed().len() > before),
        "the statement never reached the driver"
    );
    assert_eq!(status(engine, q), QueryStatus::Running);
    q
}

#[test]
fn a_cancel_the_server_never_answers_frees_the_tab() {
    let (engine, fake, key) = hanging(dev());
    let stuck = start_hanging(&engine, &fake, &key);

    engine.cancel(stuck);
    assert_eq!(settled(&engine, stuck), QueryStatus::Cancelled);
    assert!(
        messages(&engine, stuck)
            .iter()
            .any(|m| m.contains("did not answer the cancel")),
        "{:?}",
        messages(&engine, stuck)
    );

    // The next statement on the tab runs on a new connection.
    let next = engine.execute_in(&key, None, "select 1".into(), 100);
    assert!(
        matches!(settled(&engine, next), QueryStatus::Ready { .. }),
        "{:?}",
        status(&engine, next)
    );
    assert_eq!(fake.connects().len(), 3, "explorer, tab, and the tab again");
}

#[test]
fn rerunning_a_tab_whose_statement_never_ends_works() {
    // What a tab does on Run: close the previous result, then execute.
    let (engine, fake, key) = hanging(dev());
    let stuck = start_hanging(&engine, &fake, &key);
    engine.close_result(stuck);

    let next = engine.execute_in(&key, None, "select 1".into(), 100);
    assert!(
        matches!(settled(&engine, next), QueryStatus::Ready { .. }),
        "{:?}",
        status(&engine, next)
    );
    assert!(
        messages(&engine, next)
            .iter()
            .any(|m| m.contains("ran on a new connection")),
        "{:?}",
        messages(&engine, next)
    );
}

#[test]
fn a_statement_queued_behind_a_stuck_one_cancels_at_once() {
    let (engine, fake, key) = hanging(dev());
    let _stuck = start_hanging(&engine, &fake, &key);
    let queued = engine.execute_in(&key, None, "select 1".into(), 100);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(status(&engine, queued), QueryStatus::Running);

    engine.cancel(queued);
    // Long before any grace: the queued statement never reached the server.
    assert!(
        wait_until(Duration::from_millis(100), || status(&engine, queued)
            == QueryStatus::Cancelled),
        "{:?}",
        status(&engine, queued)
    );
    assert_eq!(fake.executed(), ["select hang"]);
}

#[test]
fn a_statement_queued_inside_a_lost_transaction_is_not_moved() {
    let (engine, fake, key) = hanging(ConnectionProfile {
        commit_mode: CommitMode::Manual,
        ..dev()
    });
    let stuck = start_hanging(&engine, &fake, &key);
    assert!(wait_until(TIMEOUT, || engine
        .transaction(&key)
        .is_some_and(|t| t.phase == TxPhase::Open)));
    let queued = engine.execute_in(&key, None, "select 1".into(), 100);

    engine.cancel(stuck);
    assert_eq!(settled(&engine, stuck), QueryStatus::Cancelled);
    // It belonged to the transaction the dropped connection took with it.
    let QueryStatus::Failed(e) = settled(&engine, queued) else {
        panic!("{:?}", status(&engine, queued));
    };
    assert_eq!(e.kind, ErrorKind::Connection);
    assert!(
        messages(&engine, stuck)
            .iter()
            .any(|m| m.contains("The transaction was lost")),
        "{:?}",
        messages(&engine, stuck)
    );
    assert_eq!(engine.transaction(&key).unwrap().phase, TxPhase::Idle);

    // The tab works again.
    let next = engine.execute_in(&key, None, "select 2".into(), 100);
    assert!(matches!(settled(&engine, next), QueryStatus::Ready { .. }));
}

#[test]
fn a_cancel_the_server_answers_keeps_the_connection() {
    let fake = FakeDriver::new(FakeScript {
        rows: 10,
        block_on_fetch: true,
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.set_cancel_grace(GRACE);
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    let key = engine.session("c", Some(1));
    let q = engine.execute_in(&key, None, "select 1".into(), 100);
    assert!(wait_until(TIMEOUT, || fake.blocked_fetches() == 1));
    engine.cancel(q);
    assert_eq!(settled(&engine, q), QueryStatus::Cancelled);

    std::thread::sleep(GRACE * 3);
    let next = engine.execute_in(&key, None, "select 2".into(), 100);
    // Blocks again on the fake; what matters is that it ran on the same connection.
    assert!(wait_until(TIMEOUT, || fake.blocked_fetches() == 2));
    engine.cancel(next);
    assert_eq!(settled(&engine, next), QueryStatus::Cancelled);
    assert_eq!(fake.connects().len(), 2, "explorer and tab only");
}

#[test]
fn a_cancel_that_ends_the_server_session_is_a_cancel() {
    // SQL Server cancels with `KILL`: the statement fails as a lost connection.
    let fake = FakeDriver::new(FakeScript {
        rows: 10,
        block_fetch_matching: Some("slow".into()),
        cancel_closes_connection: true,
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    let key = engine.session("c", Some(1));
    let q = engine.execute_in(&key, None, "select slow".into(), 100);
    assert!(wait_until(TIMEOUT, || fake.blocked_fetches() == 1));

    engine.cancel(q);
    assert_eq!(settled(&engine, q), QueryStatus::Cancelled);
    // A cancelled read is never run again on a new connection.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(fake.executed(), ["select slow"]);

    // The next statement reconnects instead of failing on the ended session.
    let next = engine.execute_in(&key, None, "select 1".into(), 100);
    assert!(matches!(settled(&engine, next), QueryStatus::Ready { .. }));
    assert_eq!(fake.connects().len(), 3, "explorer, tab, and the tab again");
}

#[test]
fn a_write_after_an_open_result_whose_connection_died_runs_on_a_new_one() {
    let fake = FakeDriver::new(FakeScript {
        rows: 1200,
        close_fails_matching: Some("big".into()),
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    let key = engine.session("c", Some(1));
    let open = engine.execute_in(&key, None, "select big".into(), 100);
    assert_eq!(settled(&engine, open), QueryStatus::Ready { more: true });
    assert_eq!(fake.connects().len(), 2, "explorer and tab");

    // Closing the open result finds the connection gone before the write reaches it.
    let write = engine.execute_in(&key, None, "insert into t values (1)".into(), 100);
    assert!(
        matches!(settled(&engine, write), QueryStatus::Ready { .. }),
        "{:?}",
        status(&engine, write)
    );
    assert_eq!(fake.connects().len(), 3, "the tab reconnected");
    assert_eq!(fake.executed(), ["select big", "insert into t values (1)"]);
    assert!(
        messages(&engine, write)
            .iter()
            .any(|m| m == "Connection restored"),
        "{:?}",
        messages(&engine, write)
    );
}

#[test]
fn a_write_after_a_dead_open_result_inside_a_transaction_fails() {
    let fake = FakeDriver::new(FakeScript {
        rows: 1200,
        close_fails_matching: Some("big".into()),
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(
        config("c", "fake"),
        Secrets::default(),
        ConnectionProfile {
            commit_mode: CommitMode::Manual,
            ..dev()
        },
    );
    let key = engine.session("c", Some(1));
    let open = engine.execute_in(&key, None, "select big".into(), 100);
    assert_eq!(settled(&engine, open), QueryStatus::Ready { more: true });

    let write = engine.execute_in(&key, None, "insert into t values (1)".into(), 100);
    let QueryStatus::Failed(e) = settled(&engine, write) else {
        panic!("{:?}", status(&engine, write));
    };
    assert_eq!(e.kind, ErrorKind::Connection);
    assert!(
        !fake.executed().iter().any(|s| s.starts_with("insert")),
        "{:?}",
        fake.executed()
    );
    assert!(
        messages(&engine, write)
            .iter()
            .any(|m| m.contains("The transaction was lost")),
        "{:?}",
        messages(&engine, write)
    );
}

#[test]
fn a_statement_queued_inside_a_transaction_the_connection_lost_does_not_run() {
    let fake = FakeDriver::new(FakeScript {
        affected: Some(1),
        connection_error_matching: Some("boom".into()),
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    let key = engine.session("c", Some(1));
    let begin = engine.execute_in(&key, None, "begin".into(), 100);
    assert!(matches!(settled(&engine, begin), QueryStatus::Ready { .. }));

    // Issued together: the second waits behind the first, whose connection drops.
    let lost = engine.execute_in(&key, None, "insert into t values ('boom')".into(), 100);
    let queued = engine.execute_in(&key, None, "insert into t values (2)".into(), 100);
    assert!(matches!(settled(&engine, lost), QueryStatus::Failed(_)));
    let QueryStatus::Failed(e) = settled(&engine, queued) else {
        panic!("{:?}", status(&engine, queued));
    };
    assert_eq!(e.kind, ErrorKind::Connection);
    // On a new connection it would have committed on its own, outside the transaction.
    assert!(
        !fake.executed().iter().any(|s| s.contains("(2)")),
        "{:?}",
        fake.executed()
    );
    assert_eq!(engine.transaction(&key).unwrap().phase, TxPhase::Idle);
}
