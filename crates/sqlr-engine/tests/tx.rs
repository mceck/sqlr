//! Sessions per editor tab, commit modes, read-only sessions and savepoints (§7.9).

use std::sync::Arc;

use sqlr_core::{CommitMode, ConnectionProfile, ErrorKind, Node, NodeKind, NodePath, Secrets};
use sqlr_engine::{
    DriverRegistry, Engine, LoadState, MessageLevel, QueryId, QueryStatus, SessionKey,
    SessionStatus, Slot, TxPhase,
};
use sqlr_testkit::{FakeDocDriver, FakeDocScript, FakeDriver, FakeScript};

use crate::support;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn profile(mode: CommitMode) -> ConnectionProfile {
    ConnectionProfile {
        commit_mode: mode,
        ..dev()
    }
}

fn nontransactional(profile: ConnectionProfile) -> (Engine, Arc<FakeDriver>, SessionKey) {
    let fake = FakeDriver::without_transactions(writes());
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), profile);
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
    let key = engine.session("c", Some(1));
    (engine, fake, key)
}

#[test]
fn nontransactional_manual_profile_reads_without_begin() {
    let (engine, fake, key) = nontransactional(profile(CommitMode::Manual));
    ok(&engine, &key, "select 1");
    assert_eq!(fake.executed(), ["select 1"]);
    assert_eq!(engine.transaction(&key).unwrap().mode, CommitMode::Auto);
    assert_eq!(phase(&engine, &key), TxPhase::Idle);
}

#[test]
fn nontransactional_smart_profile_writes_without_begin_or_mode_changes() {
    let (engine, fake, key) = nontransactional(profile(CommitMode::Smart));
    engine.set_commit_mode(&key, CommitMode::Manual);
    ok(&engine, &key, "insert into t values (1)");
    assert_eq!(fake.executed(), ["insert into t values (1)"]);
    assert_eq!(engine.transaction(&key).unwrap().mode, CommitMode::Auto);
    assert!(engine.open_transactions().is_empty());
}

#[test]
fn nontransactional_explicit_transaction_commands_fail_before_driver_execution() {
    let (engine, fake, key) = nontransactional(profile(CommitMode::Auto));
    for sql in ["BEGIN", "COMMIT", "ROLLBACK", "SAVEPOINT point"] {
        assert!(
            matches!(run(&engine, &key, sql), QueryStatus::Failed(e) if e.kind == ErrorKind::Unsupported)
        );
    }
    assert!(fake.executed().is_empty());
    assert!(engine.open_transactions().is_empty());
}

#[test]
fn nontransactional_profiles_and_agent_sessions_keep_read_only_policy() {
    let (engine, fake, key) = nontransactional(ConnectionProfile {
        read_only: true,
        ..profile(CommitMode::Manual)
    });
    let state = engine.transaction(&key).unwrap();
    assert_eq!(state.mode, CommitMode::Auto);
    assert!(state.read_only);
    assert!(
        matches!(run(&engine, &key, "insert into t values (1)"), QueryStatus::Failed(e) if e.kind == ErrorKind::Permission)
    );
    let agent = engine.agent_session("c", "read");
    engine.prepare_agent_session(&agent, false);
    engine.set_commit_mode(&agent, CommitMode::Smart);
    ok(&engine, &agent, "select 1");
    assert_eq!(engine.transaction(&agent).unwrap().mode, CommitMode::Auto);
    assert!(engine.transaction(&agent).unwrap().read_only);
    assert!(
        matches!(run(&engine, &agent, "insert into t values (2)"), QueryStatus::Failed(e) if e.kind == ErrorKind::Permission)
    );
    assert_eq!(fake.executed(), ["select 1"]);
}

/// An engine with the connection "c" connected under `profile`, and tab 1's session key.
fn setup(script: FakeScript, profile: ConnectionProfile) -> (Engine, Arc<FakeDriver>, SessionKey) {
    let fake = FakeDriver::new(script);
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), profile);
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
    let key = engine.session("c", Some(1));
    (engine, fake, key)
}

fn writes() -> FakeScript {
    FakeScript {
        affected: Some(1),
        ..Default::default()
    }
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

fn run(engine: &Engine, key: &SessionKey, sql: &str) -> QueryStatus {
    let q = engine.execute_in(key, None, sql.into(), 100);
    settled(engine, q)
}

fn ok(engine: &Engine, key: &SessionKey, sql: &str) {
    let st = run(engine, key, sql);
    assert!(matches!(st, QueryStatus::Ready { .. }), "{sql}: {st:?}");
}

fn phase(engine: &Engine, key: &SessionKey) -> TxPhase {
    engine.transaction(key).expect("session").phase
}

/// Waits for the session's queue to drain (a transaction command has no result to watch).
fn wait_phase(engine: &Engine, key: &SessionKey, want: TxPhase) {
    assert!(
        wait_until(TIMEOUT, || phase(engine, key) == want),
        "phase stayed {:?}",
        phase(engine, key)
    );
}

#[test]
fn session_keys_follow_the_setting() {
    let (engine, _, key) = setup(FakeScript::default(), dev());
    assert_eq!(
        key,
        SessionKey {
            conn: "c".into(),
            slot: Slot::Tab(1)
        }
    );
    assert_eq!(engine.session("c", None).slot, Slot::Meta);
}

#[test]
fn manual_mode_begins_once() {
    let (engine, fake, key) = setup(writes(), profile(CommitMode::Manual));
    ok(&engine, &key, "insert into t values (1)");
    ok(&engine, &key, "insert into t values (2)");
    assert_eq!(
        fake.executed(),
        [
            "BEGIN",
            "insert into t values (1)",
            "insert into t values (2)"
        ]
    );
    let tx = engine.transaction(&key).unwrap();
    assert_eq!(tx.phase, TxPhase::Open);
    assert_eq!(tx.statements, 2);
    assert!(tx.since_ms.is_some());
    assert_eq!(tx.mode, CommitMode::Manual);
    assert_eq!(engine.open_transactions(), vec![(key.clone(), tx)]);
}

#[test]
fn smart_mode_begins_on_first_write() {
    let (engine, fake, key) = setup(FakeScript::default(), profile(CommitMode::Smart));
    ok(&engine, &key, "select 1");
    assert_eq!(phase(&engine, &key), TxPhase::Idle);
    ok(&engine, &key, "delete from t where id = 1");
    assert_eq!(
        fake.executed(),
        ["select 1", "BEGIN", "delete from t where id = 1"]
    );
    let tx = engine.transaction(&key).unwrap();
    assert_eq!((tx.phase, tx.statements), (TxPhase::Open, 1));
}

#[test]
fn a_plsql_block_is_not_a_transaction_begin() {
    // Oracle's `BEGIN … END` is an anonymous block. In Auto mode it must not open a
    // transaction; in Smart mode a block that writes must open one like any write.
    let (engine, _, key) = setup(
        FakeScript {
            begin_is_plsql: true,
            ..Default::default()
        },
        profile(CommitMode::Auto),
    );
    ok(&engine, &key, "begin null; end;");
    assert_eq!(phase(&engine, &key), TxPhase::Idle);

    let (engine, fake, key) = setup(
        FakeScript {
            begin_is_plsql: true,
            affected: Some(1),
            ..Default::default()
        },
        profile(CommitMode::Smart),
    );
    ok(&engine, &key, "begin insert into t values (1); end;");
    assert_eq!(
        fake.executed(),
        ["BEGIN", "begin insert into t values (1); end;"]
    );
    let tx = engine.transaction(&key).unwrap();
    assert_eq!((tx.phase, tx.statements), (TxPhase::Open, 1));
}

#[test]
fn smart_returns_to_auto_after_commit() {
    let (engine, fake, key) = setup(writes(), profile(CommitMode::Smart));
    ok(&engine, &key, "update t set a = 1 where id = 1");
    engine.commit(&key);
    wait_phase(&engine, &key, TxPhase::Idle);
    ok(&engine, &key, "select 2");
    assert_eq!(
        fake.executed(),
        [
            "BEGIN",
            "update t set a = 1 where id = 1",
            "COMMIT",
            "select 2"
        ]
    );
    assert_eq!(engine.transaction(&key).unwrap().mode, CommitMode::Smart);
    // The next write opens a new transaction.
    ok(&engine, &key, "insert into t values (1)");
    assert_eq!(phase(&engine, &key), TxPhase::Open);
    engine.rollback(&key);
    wait_phase(&engine, &key, TxPhase::Idle);
    assert_eq!(fake.executed().last().map(String::as_str), Some("ROLLBACK"));
}

#[test]
fn user_begin_tracked() {
    let (engine, fake, key) = setup(writes(), dev());
    ok(&engine, &key, "begin");
    assert_eq!(phase(&engine, &key), TxPhase::Open);
    ok(&engine, &key, "insert into t values (1)");
    assert_eq!(engine.transaction(&key).unwrap().statements, 1);
    ok(&engine, &key, "commit");
    assert_eq!(phase(&engine, &key), TxPhase::Idle);
    assert_eq!(
        fake.executed(),
        ["begin", "insert into t values (1)", "commit"]
    );
}

#[test]
fn savepoints_listed_and_rolled_back() {
    let (engine, fake, key) = setup(writes(), profile(CommitMode::Manual));
    ok(&engine, &key, "insert into t values (1)");
    engine.savepoint(&key, "sp1".into());
    engine.savepoint(&key, "sp2".into());
    assert!(wait_until(TIMEOUT, || engine
        .transaction(&key)
        .unwrap()
        .savepoints
        .len()
        == 2));
    engine.rollback_to(&key, "sp1".into());
    assert!(wait_until(TIMEOUT, || fake.executed().len() == 5));
    assert_eq!(
        fake.executed()[2..],
        [
            "SAVEPOINT \"sp1\"",
            "SAVEPOINT \"sp2\"",
            "ROLLBACK TO SAVEPOINT \"sp1\""
        ]
    );
    assert!(wait_until(TIMEOUT, || engine
        .transaction(&key)
        .unwrap()
        .savepoints
        == ["sp1"]));
    // User-typed savepoint statements are tracked too.
    ok(&engine, &key, "savepoint \"later\"");
    assert_eq!(
        engine.transaction(&key).unwrap().savepoints,
        ["sp1", "later"]
    );
}

#[test]
fn typed_savepoint_before_begin_starts_the_transaction() {
    let (engine, fake, key) = setup(writes(), profile(CommitMode::Manual));
    ok(&engine, &key, "savepoint \"sp\"");
    assert_eq!(
        fake.executed(),
        ["BEGIN", "savepoint \"sp\""],
        "a savepoint must not leave the session outside its manual transaction"
    );
    assert_eq!(phase(&engine, &key), TxPhase::Open);
    assert_eq!(engine.transaction(&key).unwrap().savepoints, ["sp"]);
}

#[test]
fn smart_mode_typed_savepoint_starts_the_transaction() {
    let (engine, fake, key) = setup(writes(), profile(CommitMode::Smart));
    ok(&engine, &key, "savepoint \"sp\"");
    assert_eq!(fake.executed(), ["BEGIN", "savepoint \"sp\""]);
    assert_eq!(phase(&engine, &key), TxPhase::Open);
    assert_eq!(engine.transaction(&key).unwrap().savepoints, ["sp"]);
}

/// With no transaction open, `ROLLBACK TO` and `RELEASE` name no savepoint: the server
/// reports that, and the engine opens no transaction for them.
#[test]
fn rollback_to_and_release_before_begin_open_no_transaction() {
    for mode in [CommitMode::Manual, CommitMode::Smart] {
        let (engine, fake, key) = setup(writes(), profile(mode));
        ok(&engine, &key, "rollback to savepoint \"sp\"");
        ok(&engine, &key, "release savepoint \"sp\"");
        assert_eq!(
            fake.executed(),
            ["rollback to savepoint \"sp\"", "release savepoint \"sp\""],
            "{mode:?}"
        );
        assert_eq!(phase(&engine, &key), TxPhase::Idle, "{mode:?}");
    }
}

#[test]
fn read_only_refuses_writes_before_driver() {
    let ro = ConnectionProfile {
        read_only: true,
        ..dev()
    };
    let (engine, fake, key) = setup(writes(), ro);
    assert!(engine.transaction(&key).unwrap().read_only);
    for sql in [
        "insert into t values (1)",
        "drop table t",
        "set x = 1",
        "select 1\ndelete from t",
        "begin delete from t end",
    ] {
        let QueryStatus::Failed(e) = run(&engine, &key, sql) else {
            panic!("{sql} ran on a read-only session");
        };
        assert_eq!(e.kind, ErrorKind::Permission);
        assert_eq!(e.message, "The session is read-only");
    }
    ok(&engine, &key, "select 1");
    assert_eq!(fake.executed(), ["select 1"], "no write reached the driver");

    engine.set_read_only(&key, false);
    assert!(wait_until(TIMEOUT, || !engine
        .transaction(&key)
        .unwrap()
        .read_only));
    ok(&engine, &key, "insert into t values (1)");
}

#[test]
fn read_only_flag_follows_the_server() {
    let script = FakeScript {
        read_only_sql: true,
        fail_matching: Some("SET READ".into()),
        ..writes()
    };
    let (engine, _, key) = setup(script, dev());
    ok(&engine, &key, "select 1");
    engine.set_read_only(&key, true);
    // The server refused the switch: the session stays writable and says why.
    let mut notices = Vec::new();
    assert!(wait_until(TIMEOUT, || {
        notices.extend(engine.take_notices());
        !notices.is_empty()
    }));
    assert!(notices[0].contains("read-only"), "{notices:?}");
    assert!(!engine.transaction(&key).unwrap().read_only);
}

#[test]
fn read_only_reaches_document_connections() {
    let fake = FakeDocDriver::new(FakeDocScript::default());
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    let read_only = ConnectionProfile {
        read_only: true,
        ..dev()
    };
    engine.connect(config("d", "fakedoc"), Secrets::default(), read_only);
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("d"),
        Some(SessionStatus::Connected(_))
    )));
    assert!(
        wait_until(TIMEOUT, || fake.read_only()),
        "a read-only profile turns the document connection read-only"
    );
    let key = SessionKey {
        conn: "d".into(),
        slot: Slot::Meta,
    };
    engine.set_read_only(&key, false);
    assert!(wait_until(TIMEOUT, || !fake.read_only()));
    engine.set_read_only(&key, true);
    assert!(wait_until(TIMEOUT, || fake.read_only()));
}

#[test]
fn read_only_runs_the_dialect_sql_and_reapplies_it() {
    let script = FakeScript {
        read_only_sql: true,
        connection_error_matching: Some("kill".into()),
        ..writes()
    };
    let (engine, fake, key) = setup(script, dev());
    ok(&engine, &key, "select 1");
    engine.set_read_only(&key, true);
    assert!(wait_until(TIMEOUT, || fake.executed().len() == 2));
    assert_eq!(fake.executed(), ["select 1", "SET READ ONLY"]);
    // A lost connection reconnects read-only.
    assert!(matches!(
        run(&engine, &key, "select kill"),
        QueryStatus::Failed(_)
    ));
    ok(&engine, &key, "select 2");
    let executed = fake.executed();
    assert_eq!(
        executed[executed.len() - 2..],
        ["SET READ ONLY", "select 2"]
    );
}

#[test]
fn per_tab_sessions_run_concurrently() {
    let tree = vec![Node {
        path: NodePath(vec!["public".into()]),
        name: "public".into(),
        kind: NodeKind::Schema,
        has_children: true,
        detail: None,
    }];
    let script = FakeScript {
        rows: 10,
        block_fetch_matching: Some("slow".into()),
        tree,
        ..Default::default()
    };
    let (engine, _, tab1) = setup(script, dev());
    let tab2 = engine.session("c", Some(2));
    let slow = engine.execute_in(&tab1, None, "select slow".into(), 100);
    assert!(wait_until(TIMEOUT, || engine.transaction(&tab1).is_some()));
    let fast = engine.execute_in(&tab2, None, "select 1".into(), 100);
    assert_eq!(settled(&engine, fast), QueryStatus::Ready { more: false });
    engine.browse("c", NodePath::root());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.children("c", &NodePath::root()),
        Some(LoadState::Ready(_))
    )));
    assert_eq!(status(&engine, slow), QueryStatus::Running);
    engine.cancel(slow);
    assert_eq!(settled(&engine, slow), QueryStatus::Cancelled);
}

#[test]
fn shared_session_when_disabled() {
    let (engine, fake, _) = setup(FakeScript::default(), dev());
    engine.set_session_per_tab(false);
    let a = engine.session("c", Some(1));
    let b = engine.session("c", Some(2));
    assert_eq!(
        a,
        SessionKey {
            conn: "c".into(),
            slot: Slot::Meta
        }
    );
    assert_eq!(a, b);
    ok(&engine, &a, "select 1");
    assert_eq!(fake.open_connections(), 1, "no extra connection was opened");
}

#[test]
fn turning_sessions_per_tab_off_keeps_open_tab_sessions() {
    let (engine, _, tab1) = setup(writes(), profile(CommitMode::Manual));
    ok(&engine, &tab1, "insert into t values (1)");
    engine.set_session_per_tab(false);
    // Tab 1 still reaches its own session and its open transaction.
    assert_eq!(engine.session("c", Some(1)), tab1);
    assert_eq!(phase(&engine, &tab1), TxPhase::Open);
    // A tab that never opened a session shares the explorer's.
    assert_eq!(engine.session("c", Some(2)).slot, Slot::Meta);
    engine.close_session(&tab1);
    assert_eq!(engine.session("c", Some(1)).slot, Slot::Meta);
}

#[test]
fn close_session_closes_connection() {
    let (engine, fake, key) = setup(FakeScript::default(), dev());
    assert_eq!(fake.open_connections(), 1);
    ok(&engine, &key, "select 1");
    assert_eq!(
        fake.open_connections(),
        2,
        "the tab opened its own connection"
    );
    engine.close_session(&key);
    assert!(wait_until(TIMEOUT, || fake.open_connections() == 1));
    assert_eq!(
        engine.transaction(&key).map(|t| t.phase),
        Some(TxPhase::Idle)
    );
    // Closing the shared session (setting off) leaves the explorer connected.
    engine.close_session(&engine.session("c", None));
    assert!(matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    ));
}

#[test]
fn disconnect_closes_every_session() {
    let (engine, fake, key) = setup(FakeScript::default(), dev());
    ok(&engine, &key, "select 1");
    ok(&engine, &engine.session("c", Some(2)), "select 1");
    assert_eq!(fake.open_connections(), 3);
    engine.disconnect("c");
    assert!(wait_until(TIMEOUT, || fake.open_connections() == 0));
    let QueryStatus::Failed(e) = run(&engine, &key, "select 1") else {
        panic!("ran after disconnect")
    };
    assert_eq!(e.kind, ErrorKind::Connection);
}

#[test]
fn killed_session_clears_transaction() {
    let script = FakeScript {
        connection_error_matching: Some("kill".into()),
        ..writes()
    };
    let (engine, fake, key) = setup(script, profile(CommitMode::Manual));
    ok(&engine, &key, "insert into t values (1)");
    assert_eq!(phase(&engine, &key), TxPhase::Open);
    let q = engine.execute_in(&key, None, "select kill".into(), 100);
    let QueryStatus::Failed(e) = settled(&engine, q) else {
        panic!("expected a failure")
    };
    assert_eq!(e.kind, ErrorKind::Connection);
    let tx = engine.transaction(&key).unwrap();
    assert_eq!((tx.phase, tx.statements), (TxPhase::Idle, 0));
    let messages = engine.result(q).unwrap().read().messages.clone();
    assert!(
        messages.iter().any(|m| m.level == MessageLevel::Error
            && m.text == "The transaction was lost: server closed the connection unexpectedly"),
        "{messages:?}"
    );
    // The next run reconnects the session and opens a new transaction.
    ok(&engine, &key, "insert into t values (2)");
    let executed = fake.executed();
    assert_eq!(
        executed[executed.len() - 2..],
        ["BEGIN", "insert into t values (2)"]
    );
    assert_eq!(phase(&engine, &key), TxPhase::Open);
}

#[test]
fn postgres_failure_leaves_only_rollback() {
    let script = FakeScript {
        error_aborts_transaction: true,
        fail_matching: Some("oops".into()),
        ..writes()
    };
    let (engine, _, key) = setup(script, profile(CommitMode::Manual));
    ok(&engine, &key, "insert into t values (1)");
    assert!(matches!(
        run(&engine, &key, "select oops"),
        QueryStatus::Failed(_)
    ));
    assert_eq!(phase(&engine, &key), TxPhase::Failed);
    engine.rollback(&key);
    wait_phase(&engine, &key, TxPhase::Idle);
}

#[test]
fn commit_of_a_failed_transaction_reports_the_rollback() {
    let script = FakeScript {
        error_aborts_transaction: true,
        fail_matching: Some("oops".into()),
        ..writes()
    };
    let (engine, _, key) = setup(script, profile(CommitMode::Manual));
    ok(&engine, &key, "insert into t values (1)");
    assert!(matches!(
        run(&engine, &key, "select oops"),
        QueryStatus::Failed(_)
    ));
    engine.commit(&key);
    wait_phase(&engine, &key, TxPhase::Idle);
    let notices = engine.take_notices();
    assert!(
        notices
            .iter()
            .any(|n| n.contains("The failed transaction was rolled back")),
        "{notices:?}"
    );
}

#[test]
fn mysql_failing_ddl_ends_transaction() {
    let script = FakeScript {
        ddl_commits_transaction: true,
        fail_matching: Some("bad".into()),
        ..writes()
    };
    let (engine, _, key) = setup(script, profile(CommitMode::Manual));
    ok(&engine, &key, "insert into t values (1)");
    assert!(matches!(
        run(&engine, &key, "create table bad (a int)"),
        QueryStatus::Failed(_)
    ));
    assert_eq!(phase(&engine, &key), TxPhase::Idle);
}

#[test]
fn mysql_ddl_ends_transaction() {
    let script = FakeScript {
        ddl_commits_transaction: true,
        ..writes()
    };
    let (engine, fake, key) = setup(script, profile(CommitMode::Smart));
    ok(&engine, &key, "insert into t values (1)");
    assert_eq!(phase(&engine, &key), TxPhase::Open);
    let q = engine.execute_in(&key, None, "create table u (a int)".into(), 100);
    settled(&engine, q);
    assert_eq!(phase(&engine, &key), TxPhase::Idle);
    let messages = engine.result(q).unwrap().read().messages.clone();
    assert!(
        messages
            .iter()
            .any(|m| m.text == "MySQL committed the open transaction before DDL"),
        "{messages:?}"
    );
    // DDL alone opens no transaction that the server would commit at once.
    ok(&engine, &key, "create table v (a int)");
    assert_eq!(phase(&engine, &key), TxPhase::Idle);
    assert_eq!(fake.executed().iter().filter(|s| *s == "BEGIN").count(), 1);
}

#[test]
fn other_database_refused_while_a_transaction_is_open() {
    let script = FakeScript {
        own_database: Some("app".into()),
        databases: vec!["other".into()],
        ..writes()
    };
    let (engine, fake, _) = setup(script, profile(CommitMode::Manual));
    // The shared session (sessions per tab off) runs statements for any database.
    let key = engine.session("c", None);
    ok(&engine, &key, "insert into t values (1)");
    let q = engine.execute_in(&key, Some("other"), "insert into u values (1)".into(), 100);
    let QueryStatus::Failed(e) = settled(&engine, q) else {
        panic!("ran outside the open transaction")
    };
    assert_eq!(
        e.message,
        "A transaction is open on app; commit or roll back first"
    );
    assert_eq!(engine.transaction(&key).unwrap().statements, 1);
    assert!(!fake.executed().iter().any(|s| s.contains("into u")));
    // Once it ends, the other database runs (in its own transaction).
    engine.commit(&key);
    wait_phase(&engine, &key, TxPhase::Idle);
    let q = engine.execute_in(&key, Some("other"), "insert into u values (1)".into(), 100);
    assert!(matches!(settled(&engine, q), QueryStatus::Ready { .. }));
}

#[test]
fn a_page_read_opens_no_manual_transaction() {
    let (engine, fake, key) = setup(writes(), profile(CommitMode::Manual));
    let q = engine.read_in(&key, None, "select * from t".into(), 100);
    assert!(matches!(settled(&engine, q), QueryStatus::Ready { .. }));
    assert_eq!(fake.executed(), ["select * from t"]);
    assert_eq!(phase(&engine, &key), TxPhase::Idle);
    // A write through it still follows the commit mode.
    let q = engine.read_in(&key, None, "insert into t values (1)".into(), 100);
    assert!(matches!(settled(&engine, q), QueryStatus::Ready { .. }));
    assert_eq!(phase(&engine, &key), TxPhase::Open);
}

#[test]
fn transactions_say_whether_they_wrote() {
    let (engine, _, key) = setup(writes(), profile(CommitMode::Manual));
    ok(&engine, &key, "select 1");
    let tx = engine.transaction(&key).unwrap();
    assert_eq!(tx.phase, TxPhase::Open);
    assert!(!tx.wrote && !tx.blocks_ddl());
    ok(&engine, &key, "insert into t values (1)");
    let tx = engine.transaction(&key).unwrap();
    assert!(tx.wrote && tx.blocks_ddl());
    engine.commit(&key);
    wait_phase(&engine, &key, TxPhase::Idle);
    assert!(!engine.transaction(&key).unwrap().wrote);
    // A savepoint is enough.
    ok(&engine, &key, "select 1");
    engine.savepoint(&key, "sp1".into());
    assert!(wait_until(TIMEOUT, || engine
        .transaction(&key)
        .unwrap()
        .blocks_ddl()));
}

#[test]
fn commit_mode_switch_to_auto_refused_while_open() {
    let (engine, _, key) = setup(writes(), profile(CommitMode::Manual));
    ok(&engine, &key, "insert into t values (1)");
    engine.set_commit_mode(&key, CommitMode::Auto);
    assert_eq!(engine.transaction(&key).unwrap().mode, CommitMode::Manual);
    engine.set_commit_mode(&key, CommitMode::Smart);
    assert_eq!(engine.transaction(&key).unwrap().mode, CommitMode::Smart);
    engine.commit(&key);
    wait_phase(&engine, &key, TxPhase::Idle);
    engine.set_commit_mode(&key, CommitMode::Auto);
    assert_eq!(engine.transaction(&key).unwrap().mode, CommitMode::Auto);
    // A tab that has not run yet takes the mode it is given.
    let tab9 = engine.session("c", Some(9));
    engine.set_commit_mode(&tab9, CommitMode::Manual);
    assert_eq!(engine.transaction(&tab9).unwrap().mode, CommitMode::Manual);
}

#[test]
fn session_settings_reset_on_disconnect_and_profile_change() {
    let (engine, _, tab) = setup(FakeScript::default(), dev());
    engine.set_commit_mode(&tab, CommitMode::Manual);
    engine.set_read_only(&tab, true);
    engine.disconnect("c");
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    let tx = engine.transaction(&tab).unwrap();
    assert_eq!((tx.mode, tx.read_only), (CommitMode::Auto, false));

    // Without a disconnect, a different profile also starts over from it.
    engine.set_commit_mode(&tab, CommitMode::Manual);
    engine.connect(
        config("c", "fake"),
        Secrets::default(),
        profile(CommitMode::Smart),
    );
    assert_eq!(engine.transaction(&tab).unwrap().mode, CommitMode::Smart);
}

#[test]
fn idle_timeout_reported() {
    let quick = ConnectionProfile {
        idle_tx_timeout_min: Some(0),
        ..profile(CommitMode::Manual)
    };
    let (engine, _, key) = setup(writes(), quick);
    ok(&engine, &key, "insert into t values (1)");
    assert!(engine.transaction(&key).unwrap().idle_timeout);
    let (engine, _, key) = setup(writes(), profile(CommitMode::Manual));
    ok(&engine, &key, "insert into t values (1)");
    assert!(!engine.transaction(&key).unwrap().idle_timeout);
}

#[test]
fn messages_record_each_statement() {
    let (engine, _, key) = setup(writes(), profile(CommitMode::Manual));
    let q = engine.execute_in(&key, None, "insert into t values (1)\n-- more".into(), 100);
    settled(&engine, q);
    let messages = engine.result(q).unwrap().read().messages.clone();
    let texts: Vec<&str> = messages.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(texts[0], "Transaction started");
    assert!(
        texts[1].starts_with("insert into t values (1) · 1 rows affected · "),
        "{texts:?}"
    );
}
