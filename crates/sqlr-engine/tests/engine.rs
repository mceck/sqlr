use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sqlr_core::{DriverError, ErrorKind, Node, NodeKind, NodePath, Secrets};
use sqlr_engine::{DriverRegistry, Engine, LoadState, QueryId, QueryStatus, SessionStatus};
use sqlr_testkit::{FakeDriver, FakeScript};

use crate::support;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn engine_with(fake: Arc<FakeDriver>, repaint: sqlr_engine::Repaint) -> Engine {
    let mut drivers = DriverRegistry::default();
    drivers.register(fake);
    Engine::new(drivers, repaint)
}

/// An engine with one fake driver and the session "c" already `Connected`.
fn connected(script: FakeScript) -> (Engine, Arc<FakeDriver>) {
    let fake = FakeDriver::new(script);
    let engine = engine_with(fake.clone(), no_repaint());
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

fn status(engine: &Engine, q: QueryId) -> QueryStatus {
    engine
        .result(q)
        .expect("result exists")
        .read()
        .status
        .clone()
}

fn rows(engine: &Engine, q: QueryId) -> usize {
    engine
        .result(q)
        .expect("result exists")
        .read()
        .rows
        .row_count()
}

fn cut_short(engine: &Engine, q: QueryId) -> bool {
    engine.result(q).expect("result exists").read().cut_short
}

/// Waits until the result is no longer `Running`, then returns its status and row count.
fn settled(engine: &Engine, q: QueryId) -> (QueryStatus, usize) {
    assert!(
        wait_until(TIMEOUT, || status(engine, q) != QueryStatus::Running),
        "query {q:?} still running"
    );
    (status(engine, q), rows(engine, q))
}

fn node(path: &[&str], kind: NodeKind) -> Node {
    Node {
        path: NodePath(path.iter().map(|s| s.to_string()).collect()),
        name: path.last().unwrap().to_string(),
        kind,
        has_children: kind == NodeKind::Schema,
        detail: None,
    }
}

#[test]
fn connect_success_and_failure() {
    let engine = engine_with(FakeDriver::new(FakeScript::default()), no_repaint());
    // Both fakes have the driver id "fake", so the failing one gets its own engine.
    let bad = FakeDriver::new(FakeScript {
        connect_error: Some(DriverError::new(ErrorKind::Auth, "bad password")),
        ..Default::default()
    });
    let bad_engine = engine_with(bad, no_repaint());

    engine.connect(config("good", "fake"), Secrets::default(), dev());
    bad_engine.connect(config("bad", "fake"), Secrets::default(), dev());

    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("good"),
        Some(SessionStatus::Connected(_))
    )));
    assert!(wait_until(TIMEOUT, || matches!(
        bad_engine.session_status("bad"),
        Some(SessionStatus::Failed(_))
    )));
    let Some(SessionStatus::Failed(e)) = bad_engine.session_status("bad") else {
        unreachable!()
    };
    assert_eq!(e.kind, ErrorKind::Auth);
    assert_eq!(engine.session_status("missing"), None);
}

#[test]
fn browse_caches_children() {
    let tree = vec![
        node(&["public"], NodeKind::Schema),
        node(&["public", "users"], NodeKind::Table),
    ];
    let fake = FakeDriver::new(FakeScript {
        tree: tree.clone(),
        ..Default::default()
    });
    let engine = engine_with(fake.clone(), no_repaint());
    // Browse in the same frame as connect: it waits for the connect attempt (ruling R4).
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    engine.browse("c", NodePath::root());

    assert!(wait_until(TIMEOUT, || matches!(
        engine.children("c", &NodePath::root()),
        Some(LoadState::Ready(_))
    )));
    assert_eq!(
        engine.children("c", &NodePath::root()),
        Some(LoadState::Ready(vec![tree[0].clone()]))
    );

    engine.browse("c", NodePath::root());
    assert_eq!(fake.browse_calls(), 1);

    let users = &tree[1];
    let sql = engine.open_data_sql("c", users, Some(10));
    assert_eq!(sql.as_deref(), Some("SELECT * FROM users LIMIT 10"));
}

#[test]
fn executes_on_another_database() {
    let (engine, fake) = connected(FakeScript {
        rows: 2,
        databases: vec!["other".into()],
        ..Default::default()
    });
    let users = Node {
        path: NodePath(
            ["other", "public", "tables", "users"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ),
        name: "users".into(),
        kind: NodeKind::Table,
        has_children: true,
        detail: None,
    };
    let (database, sql) = engine.open_data_target("c", &users, None).unwrap();
    assert_eq!(database.as_deref(), Some("other"));
    assert_eq!(sql, "SELECT * FROM users");

    let q = engine.execute_on("c", database.as_deref(), sql, 500);
    assert_eq!(settled(&engine, q), (QueryStatus::Ready { more: false }, 2));
    assert_eq!(
        fake.executed_on(),
        [(Some("other".to_string()), "SELECT * FROM users".to_string())]
    );

    // The session keeps its own connection for queries without a database.
    let base = engine.execute("c", "SELECT 1".into(), 500);
    assert_eq!(
        settled(&engine, base),
        (QueryStatus::Ready { more: false }, 2)
    );
    assert_eq!(
        fake.executed_on().last(),
        Some(&(None, "SELECT 1".to_string()))
    );
}

#[test]
fn unknown_database_fails_only_the_query() {
    let (engine, _fake) = connected(FakeScript::default());
    let q = engine.execute_on("c", Some("nope"), "SELECT 1".into(), 500);
    let QueryStatus::Failed(e) = settled(&engine, q).0 else {
        panic!("expected Failed, got {:?}", status(&engine, q));
    };
    assert_eq!(e.kind, ErrorKind::Unsupported);
    assert!(
        matches!(
            engine.session_status("c"),
            Some(SessionStatus::Connected(_))
        ),
        "a missing sibling database must not fail the session"
    );
}

#[test]
fn execute_first_page_then_more() {
    let (engine, _fake) = connected(FakeScript {
        rows: 1200,
        ..Default::default()
    });
    let q = engine.execute("c", "SELECT n".into(), 500);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: true }, 500)
    );

    engine.fetch_more(q);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: true }, 1000)
    );

    engine.fetch_more(q);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: false }, 1200)
    );

    let result = engine.result(q).unwrap();
    let r = result.read();
    assert!(!r.cut_short, "an exhausted stream is complete");
    assert_eq!(r.sql, "SELECT n");
    assert_eq!(r.page_size, 500);
    assert_eq!(r.affected, None);
}

#[test]
fn fetch_all_streams_to_end() {
    let (engine, fake) = connected(FakeScript {
        rows: 5000,
        ..Default::default()
    });
    let q = engine.execute("c", "SELECT n".into(), 100);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: true }, 100)
    );

    engine.fetch_all(q);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: false }, 5000)
    );
    assert_eq!(fake.closed_streams(), 1, "an exhausted stream is closed");
    assert!(!cut_short(&engine, q));
}

#[test]
fn row_cap_zero_means_no_cap() {
    let (engine, _fake) = connected(FakeScript {
        rows: 3000,
        ..Default::default()
    });
    engine.set_row_cap(0);
    let q = engine.execute("c", "SELECT n".into(), 500);
    settled(&engine, q);
    engine.fetch_all(q);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: false }, 3000)
    );
}

#[test]
fn connection_settings_override_the_global_row_cap_and_sessions() {
    let (engine, _fake) = connected(FakeScript {
        rows: 3000,
        ..Default::default()
    });
    engine.set_row_cap(1000);
    engine.set_session_per_tab(true);
    // The connection "c" lifts the cap and shares one session; another id is unaffected.
    engine.set_connection_settings(
        "c",
        sqlr_engine::ConnectionSettings {
            row_cap: Some(0),
            session_per_tab: Some(false),
            ..Default::default()
        },
    );
    let q = engine.execute("c", "SELECT n".into(), 500);
    settled(&engine, q);
    engine.fetch_all(q);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: false }, 3000)
    );
    assert_eq!(engine.session("c", Some(7)).slot, sqlr_engine::Slot::Meta);
    assert_eq!(
        engine.session("other", Some(7)).slot,
        sqlr_engine::Slot::Tab(7)
    );
    // Clearing the override returns to the global values.
    engine.set_connection_settings("c", Default::default());
    assert_eq!(engine.session("c", Some(7)).slot, sqlr_engine::Slot::Tab(7));
}

#[test]
fn memory_budget_spills_without_losing_rows_or_invalidating_snapshots() {
    let (engine, fake) = connected(FakeScript {
        rows: 5000,
        ..Default::default()
    });
    engine.set_result_memory_limits(1500, 10_000);
    let q = engine.execute("c", "SELECT n".into(), 100);
    assert_eq!(settled(&engine, q).1, 100);
    let snapshot = engine.result(q).unwrap().read().rows.clone();
    engine.fetch_more(q);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: true }, 200)
    );
    engine.fetch_all(q);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: false }, 5000)
    );
    let result = engine.result(q).unwrap();
    let r = result.read();
    assert!(r.rows.resident_byte_size() <= 1500);
    assert!(!r.cut_short);
    assert!(!r.messages.iter().any(|m| m.text.contains("truncated")));
    for row in 0..5000 {
        assert_eq!(r.rows.cell(row, 0), Some(sqlr_core::Cell::Int(row as i64)));
    }
    let spilled_snapshot = r.rows.clone();
    drop(r);
    drop(result);
    engine.close_result(q);
    assert_eq!(snapshot.cell(99, 0), Some(sqlr_core::Cell::Int(99)));
    assert_eq!(
        spilled_snapshot.cell(4999, 0),
        Some(sqlr_core::Cell::Int(4999))
    );
    assert_eq!(fake.closed_streams(), 1);
}

#[test]
fn oversized_first_chunk_spills_directly_and_fetch_all_continues() {
    let (engine, fake) = connected(FakeScript {
        rows: 5000,
        ..Default::default()
    });
    engine.set_result_memory_limits(1, 1);
    let q = engine.execute("c", "SELECT n".into(), 100);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: true }, 100)
    );
    engine.fetch_all(q);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: false }, 5000)
    );
    assert!(!cut_short(&engine, q));
    assert_eq!(
        engine.result(q).unwrap().read().rows.resident_byte_size(),
        0
    );
    assert_eq!(fake.closed_streams(), 1);
}

#[test]
fn global_memory_budget_spills_older_results_and_keeps_both_readable() {
    let (engine, _) = connected(FakeScript {
        rows: 5000,
        ..Default::default()
    });
    engine.set_result_memory_limits(10_000, 1500);
    let first = engine.execute("c", "SELECT n".into(), 100);
    assert_eq!(settled(&engine, first).1, 100);
    let second = engine.execute("c", "SELECT n".into(), 100);
    assert_eq!(settled(&engine, second).1, 100);
    assert_eq!(
        engine
            .result(first)
            .unwrap()
            .read()
            .rows
            .resident_byte_size(),
        0
    );
    assert_eq!(
        engine.result(first).unwrap().read().rows.cell(99, 0),
        Some(sqlr_core::Cell::Int(99))
    );
    engine.close_result(first);
    let third = engine.execute("c", "SELECT n".into(), 100);
    assert_eq!(settled(&engine, third).1, 100);
}

#[test]
fn snapshots_keep_their_memory_reservations_after_a_result_closes() {
    let (engine, _) = connected(FakeScript {
        rows: 5000,
        ..Default::default()
    });
    engine.set_result_memory_limits(10_000, 1500);
    let first = engine.execute("c", "SELECT n".into(), 100);
    settled(&engine, first);
    let snapshot = engine.result(first).unwrap().read().rows.clone();
    engine.close_result(first);
    let second = engine.execute("c", "SELECT n".into(), 100);
    assert_eq!(settled(&engine, second).1, 100);
    assert_eq!(
        engine
            .result(second)
            .unwrap()
            .read()
            .rows
            .resident_byte_size(),
        0
    );
    assert_eq!(snapshot.cell(99, 0), Some(sqlr_core::Cell::Int(99)));
    drop(snapshot);
    let third = engine.execute("c", "SELECT n".into(), 100);
    settled(&engine, third);
    assert!(
        engine
            .result(third)
            .unwrap()
            .read()
            .rows
            .resident_byte_size()
            > 0
    );
}

#[test]
fn cancelling_a_spilled_fetch_keeps_received_rows_readable() {
    let (engine, _) = connected(FakeScript {
        rows: 500_000,
        ..Default::default()
    });
    engine.set_result_memory_limits(1, 1);
    engine.set_row_cap(0);
    let q = engine.execute("c", "SELECT n".into(), 100);
    settled(&engine, q);
    engine.fetch_all(q);
    assert!(wait_until(TIMEOUT, || rows(&engine, q) >= 500));
    engine.cancel(q);
    assert_eq!(settled(&engine, q).0, QueryStatus::Cancelled);
    let result = engine.result(q).unwrap();
    let snapshot = result.read().rows.clone();
    assert_eq!(snapshot.resident_byte_size(), 0);
    assert!(snapshot.row_count() >= 500);
    engine.close_result(q);
    drop(result);
    let last = snapshot.row_count() - 1;
    assert_eq!(
        snapshot.cell(last, 0),
        Some(sqlr_core::Cell::Int(last as i64))
    );
}

#[test]
fn concurrent_results_spill_under_the_shared_budget_without_losing_rows() {
    let (engine, _) = connected(FakeScript {
        rows: 5000,
        ..Default::default()
    });
    engine.connect(config("other", "fake"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("other"),
        Some(SessionStatus::Connected(_))
    )));
    engine.set_result_memory_limits(1500, 1500);
    let first = engine.execute("c", "SELECT n".into(), 100);
    let second = engine.execute("other", "SELECT n".into(), 100);
    settled(&engine, first);
    settled(&engine, second);
    engine.fetch_all(first);
    engine.fetch_all(second);
    assert_eq!(
        settled(&engine, first),
        (QueryStatus::Ready { more: false }, 5000)
    );
    assert_eq!(
        settled(&engine, second),
        (QueryStatus::Ready { more: false }, 5000)
    );
    let first = engine.result(first).unwrap();
    let second = engine.result(second).unwrap();
    assert!(
        first.read().rows.resident_byte_size() + second.read().rows.resident_byte_size() <= 1500
    );
    for result in [first, second] {
        let result = result.read();
        assert_eq!(result.rows.cell(0, 0), Some(sqlr_core::Cell::Int(0)));
        assert_eq!(result.rows.cell(4999, 0), Some(sqlr_core::Cell::Int(4999)));
    }
}

#[test]
fn zero_memory_limits_allow_unlimited_results() {
    let (engine, _) = connected(FakeScript {
        rows: 5000,
        ..Default::default()
    });
    engine.set_result_memory_limits(0, 0);
    let q = engine.execute("c", "SELECT n".into(), 100);
    settled(&engine, q);
    engine.fetch_all(q);
    assert_eq!(settled(&engine, q).1, 5000);
}

#[test]
fn fetch_all_stops_at_row_cap() {
    let (engine, fake) = connected(FakeScript {
        rows: 5000,
        ..Default::default()
    });
    engine.set_row_cap(1000);
    let q = engine.execute("c", "SELECT n".into(), 500);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: true }, 500)
    );

    engine.fetch_all(q);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: false }, 1000),
        "the fetch stops at the cap, not the stream's end"
    );
    assert_eq!(fake.closed_streams(), 1, "the rest of the stream is closed");
    let messages: Vec<String> = engine
        .result(q)
        .unwrap()
        .read()
        .messages
        .iter()
        .map(|m| m.text.clone())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m == "Result truncated at the row cap (1000 rows)"),
        "{messages:?}"
    );
}

#[test]
fn page_size_change_applies_to_next_fetch() {
    let (engine, _fake) = connected(FakeScript {
        rows: 1200,
        ..Default::default()
    });
    let q = engine.execute("c", "SELECT n".into(), 500);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: true }, 500)
    );

    engine.set_page_size(q, 100);
    engine.fetch_more(q);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: true }, 600)
    );
    assert_eq!(engine.result(q).unwrap().read().page_size, 100);
}

#[test]
fn cancel_running_query() {
    let (engine, fake) = connected(FakeScript {
        rows: 10,
        block_on_fetch: true,
        ..Default::default()
    });
    let q = engine.execute("c", "SELECT pg_sleep(60)".into(), 500);
    // The query is running once the driver has received it.
    assert!(wait_until(TIMEOUT, || fake.executed().len() == 1));
    assert_eq!(status(&engine, q), QueryStatus::Running);

    engine.cancel(q);
    assert!(
        wait_until(Duration::from_secs(1), || status(&engine, q)
            == QueryStatus::Cancelled),
        "status: {:?}",
        status(&engine, q)
    );
    assert!(wait_until(TIMEOUT, || fake.closed_streams() == 1));
    assert!(
        !cut_short(&engine, q),
        "an explicit cancel is not cut short"
    );
    // Cancelling a query that is no longer running is a no-op.
    engine.cancel(q);
    assert_eq!(status(&engine, q), QueryStatus::Cancelled);
}

#[test]
fn new_query_closes_previous_stream() {
    let (engine, fake) = connected(FakeScript {
        rows: 1200,
        ..Default::default()
    });
    let first = engine.execute("c", "SELECT 1".into(), 500);
    assert_eq!(
        settled(&engine, first),
        (QueryStatus::Ready { more: true }, 500)
    );

    let second = engine.execute("c", "SELECT 2".into(), 500);
    assert_eq!(
        settled(&engine, second),
        (QueryStatus::Ready { more: true }, 500)
    );
    assert_eq!(
        status(&engine, first),
        QueryStatus::Ready { more: false },
        "the previous result is complete with no more rows"
    );
    assert_eq!(rows(&engine, first), 500);
    assert!(
        cut_short(&engine, first),
        "the previous result was cut short"
    );
    assert!(!cut_short(&engine, second));
    assert!(fake.closed_streams() >= 1);

    // fetch_more on the closed result is ignored.
    engine.fetch_more(first);
    assert_eq!(status(&engine, first), QueryStatus::Ready { more: false });
}

#[test]
fn connection_error_marks_session_failed() {
    let (engine, fake) = connected(FakeScript {
        rows: 1200,
        fail_after_chunks: Some((1, DriverError::new(ErrorKind::Connection, "server closed"))),
        ..Default::default()
    });
    let q = engine.execute("c", "SELECT n".into(), 500);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: true }, 500)
    );

    engine.fetch_more(q);
    let (st, n) = settled(&engine, q);
    let QueryStatus::Failed(e) = st else {
        panic!("expected Failed, got {st:?}")
    };
    assert_eq!(e.kind, ErrorKind::Connection);
    assert_eq!(n, 500, "rows fetched before the failure are kept");
    assert!(matches!(
        engine.session_status("c"),
        Some(SessionStatus::Failed(e)) if e.kind == ErrorKind::Connection
    ));
    assert!(wait_until(TIMEOUT, || fake.closed_streams() == 1));

    // The session can be reconnected.
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
}

#[test]
fn execute_when_disconnected_fails() {
    let (engine, _fake) = connected(FakeScript {
        rows: 1200,
        ..Default::default()
    });
    let open = engine.execute("c", "SELECT n".into(), 500);
    assert_eq!(
        settled(&engine, open),
        (QueryStatus::Ready { more: true }, 500)
    );

    engine.disconnect("c");
    assert_eq!(
        engine.session_status("c"),
        Some(SessionStatus::Disconnected)
    );
    assert!(wait_until(TIMEOUT, || status(&engine, open)
        == QueryStatus::Ready { more: false }));
    assert!(
        cut_short(&engine, open),
        "a disconnect cuts the open result short"
    );

    let not_connected = DriverError::new(ErrorKind::Connection, "not connected");
    let q = engine.execute("c", "SELECT n".into(), 500);
    assert_eq!(
        settled(&engine, q).0,
        QueryStatus::Failed(not_connected.clone())
    );
    let unknown = engine.execute("nope", "SELECT n".into(), 500);
    assert_eq!(
        settled(&engine, unknown).0,
        QueryStatus::Failed(not_connected)
    );
}

#[test]
fn repaint_called_on_changes() {
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    let fake = FakeDriver::new(FakeScript::default());
    let engine = engine_with(
        fake,
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || count.load(Ordering::SeqCst) > 0));
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
}

#[test]
fn new_query_interrupts_fetch_all_of_previous() {
    // Far more rows than could be fetched before the second execute is issued.
    const TOTAL: usize = 100_000_000;
    let (engine, _fake) = connected(FakeScript {
        rows: TOTAL,
        ..Default::default()
    });
    let first = engine.execute("c", "SELECT 1".into(), 10);
    assert_eq!(
        settled(&engine, first),
        (QueryStatus::Ready { more: true }, 10)
    );

    engine.fetch_all(first);
    let second = engine.execute("c", "SELECT 2".into(), 500);
    assert_eq!(
        settled(&engine, second),
        (QueryStatus::Ready { more: true }, 500),
        "the new query does not wait for the previous fetch_all"
    );
    let (st, n) = settled(&engine, first);
    assert_eq!(st, QueryStatus::Ready { more: false });
    assert!(n < TOTAL, "the previous result stopped early ({n} rows)");
    assert!(cut_short(&engine, first));
}

#[test]
fn back_to_back_executes_run_in_issue_order() {
    // Ordering bugs are races, so repeat to give a wrong order many chances to show up.
    for _ in 0..20 {
        let fake = FakeDriver::new(FakeScript {
            rows: 1200,
            ..Default::default()
        });
        let engine = engine_with(fake.clone(), no_repaint());
        // Both executes are issued while the session is still connecting (ruling R4).
        engine.connect(config("c", "fake"), Secrets::default(), dev());
        let first = engine.execute("c", "SELECT 1".into(), 500);
        let second = engine.execute("c", "SELECT 2".into(), 500);

        assert_eq!(
            settled(&engine, second),
            (QueryStatus::Ready { more: true }, 500),
            "the latest query is the one left streaming"
        );
        assert_eq!(
            settled(&engine, first).0,
            QueryStatus::Ready { more: false }
        );
        assert_eq!(fake.executed(), ["SELECT 1", "SELECT 2"]);
    }
}

#[test]
fn session_database_names_the_own_database() {
    let (engine, _) = connected(FakeScript {
        own_database: Some("app".into()),
        ..Default::default()
    });
    assert_eq!(engine.session_database("c").as_deref(), Some("app"));
    assert_eq!(engine.session_database("missing"), None);
    let (engine, _) = connected(FakeScript::default());
    assert_eq!(engine.session_database("c"), None);
}
