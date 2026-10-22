use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlr_core::{
    DriverError, ErrorKind, HistoryEntry, HistoryOutcome, HistoryQuery, Secrets, Snippet,
};
use sqlr_engine::{
    DriverRegistry, Engine, HistoryChannel, LoadState, QueryId, QueryStatus, SessionStatus,
};
use sqlr_store::Db;
use sqlr_testkit::{FakeDriver, FakeScript};

mod support;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn engine_on(script: FakeScript, db: Arc<Db>) -> Engine {
    let mut drivers = DriverRegistry::default();
    drivers.register(FakeDriver::new(script));
    Engine::with_db(drivers, no_repaint(), db)
}

/// An engine on `db` with the session "c" already `Connected`.
fn connected(script: FakeScript, db: Arc<Db>) -> Engine {
    let engine = engine_on(script, db);
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(
        wait_until(TIMEOUT, || matches!(
            engine.session_status("c"),
            Some(SessionStatus::Connected(_))
        )),
        "session never connected: {:?}",
        engine.session_status("c")
    );
    engine
}

fn memory_db() -> Arc<Db> {
    Arc::new(Db::open_in_memory().unwrap())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Every stored history entry, newest first, read straight from the database.
fn stored(db: &Db) -> Vec<HistoryEntry> {
    db.search_history(&HistoryQuery::default()).unwrap()
}

/// Waits until the database holds exactly `n` history entries, then returns them.
fn wait_stored(db: &Db, n: usize) -> Vec<HistoryEntry> {
    assert!(
        wait_until(TIMEOUT, || stored(db).len() == n),
        "expected {n} history entries, found {:?}",
        stored(db)
    );
    stored(db)
}

fn settle(engine: &Engine, q: QueryId) -> QueryStatus {
    let status = || engine.result(q).unwrap().read().status.clone();
    assert!(
        wait_until(TIMEOUT, || status() != QueryStatus::Running),
        "query {q:?} still running"
    );
    status()
}

fn query(text: &str) -> HistoryQuery {
    HistoryQuery {
        text: text.into(),
        ..Default::default()
    }
}

/// The channel's entries once loaded; `None` while loading or never searched.
fn view_entries(engine: &Engine, channel: HistoryChannel) -> Option<Arc<Vec<HistoryEntry>>> {
    match engine.history(channel)?.entries {
        LoadState::Ready(entries) => Some(entries),
        _ => None,
    }
}

fn sqls(entries: &[HistoryEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.sql.as_str()).collect()
}

fn snippet(id: &str, folder: &str, name: &str) -> Snippet {
    Snippet {
        id: id.into(),
        name: name.into(),
        folder: folder.into(),
        conn_id: None,
        sql: format!("SELECT '{name}'"),
        updated_at_ms: 1,
    }
}

fn snippet_ids(engine: &Engine) -> Vec<String> {
    engine.snippets().iter().map(|s| s.id.clone()).collect()
}

fn read_only_db(path: &Path) -> Arc<Db> {
    drop(Db::open(path).unwrap());
    Arc::new(Db::open_read_only(path).unwrap())
}

#[test]
fn execute_records_history_once() {
    let db = memory_db();
    let engine = connected(
        FakeScript {
            rows: 1200,
            ..Default::default()
        },
        db.clone(),
    );
    let before = now_ms();
    let q = engine.execute("c", "SELECT n FROM big".into(), 500);
    assert_eq!(settle(&engine, q), QueryStatus::Ready { more: true });
    let after = now_ms();

    let entries = wait_stored(&db, 1);
    let e = &entries[0];
    assert_eq!(e.conn_id, "c");
    assert_eq!(e.sql, "SELECT n FROM big");
    assert_eq!(e.outcome, HistoryOutcome::Rows(500));
    assert!(
        (before..=after).contains(&e.started_at_ms),
        "started_at_ms {} not in {before}..={after}",
        e.started_at_ms
    );
    let elapsed = engine.result(q).unwrap().read().elapsed;
    assert_eq!(e.duration_ms, elapsed.as_millis() as u64);

    engine.fetch_more(q);
    assert_eq!(settle(&engine, q), QueryStatus::Ready { more: true });
    // Writes land in order, so once the next query is stored the fetch would have been too.
    let q2 = engine.execute("c", "SELECT 2".into(), 500);
    settle(&engine, q2);
    let entries = wait_stored(&db, 2);
    assert_eq!(sqls(&entries), ["SELECT 2", "SELECT n FROM big"]);
}

#[test]
fn failed_and_affected_outcomes() {
    let db = memory_db();
    let failing = connected(
        FakeScript {
            query_error: Some(DriverError::new(ErrorKind::Other, "syntax error")),
            ..Default::default()
        },
        db.clone(),
    );
    let q = failing.execute("c", "SELEC 1".into(), 500);
    assert!(matches!(settle(&failing, q), QueryStatus::Failed(_)));
    let entries = wait_stored(&db, 1);
    assert_eq!(
        entries[0].outcome,
        HistoryOutcome::Failed("syntax error".into())
    );

    // A query on a connection that is not connected is recorded too.
    failing.execute("other", "SELECT 1".into(), 500);
    let entries = wait_stored(&db, 2);
    let not_connected = entries.iter().find(|e| e.conn_id == "other").unwrap();
    assert_eq!(
        not_connected.outcome,
        HistoryOutcome::Failed("not connected".into())
    );
    drop(failing);

    let db = memory_db();
    let affecting = connected(
        FakeScript {
            affected: Some(3),
            ..Default::default()
        },
        db.clone(),
    );
    let q = affecting.execute("c", "DELETE FROM t".into(), 500);
    assert_eq!(settle(&affecting, q), QueryStatus::Ready { more: false });
    let entries = wait_stored(&db, 1);
    assert_eq!(entries[0].outcome, HistoryOutcome::Affected(3));
    drop(affecting);

    let db = memory_db();
    let blocking = connected(
        FakeScript {
            rows: 10,
            block_on_fetch: true,
            ..Default::default()
        },
        db.clone(),
    );
    let q = blocking.execute("c", "SELECT pg_sleep(60)".into(), 500);
    // Cancelled before or after the driver receives it, the query ends `Cancelled`.
    blocking.cancel(q);
    assert_eq!(settle(&blocking, q), QueryStatus::Cancelled);
    let entries = wait_stored(&db, 1);
    assert_eq!(entries[0].outcome, HistoryOutcome::Cancelled);
}

#[test]
fn search_by_channel() {
    let db = memory_db();
    let engine = connected(
        FakeScript {
            rows: 1,
            ..Default::default()
        },
        db.clone(),
    );
    assert!(engine.history(HistoryChannel::Panel).is_none());
    for sql in ["SELECT * FROM orders", "SELECT * FROM users"] {
        let q = engine.execute("c", sql.into(), 500);
        settle(&engine, q);
    }
    wait_stored(&db, 2);

    engine.search_history(HistoryChannel::Panel, query("orders"));
    engine.search_history(HistoryChannel::Palette, query(""));
    assert_eq!(
        engine.history(HistoryChannel::Panel).unwrap().query,
        query("orders")
    );
    assert!(wait_until(TIMEOUT, || {
        view_entries(&engine, HistoryChannel::Panel).is_some()
            && view_entries(&engine, HistoryChannel::Palette).is_some()
    }));
    let panel = view_entries(&engine, HistoryChannel::Panel).unwrap();
    let palette = view_entries(&engine, HistoryChannel::Palette).unwrap();
    assert_eq!(sqls(&panel), ["SELECT * FROM orders"]);
    assert_eq!(
        sqls(&palette),
        ["SELECT * FROM users", "SELECT * FROM orders"]
    );

    // A new query replaces the channel's view; the old results are never shown for it.
    engine.search_history(HistoryChannel::Panel, query("users"));
    assert!(wait_until(TIMEOUT, || view_entries(
        &engine,
        HistoryChannel::Panel
    )
    .is_some_and(|e| sqls(&e) == ["SELECT * FROM users"])));
    assert_eq!(
        engine.history(HistoryChannel::Panel).unwrap().query,
        query("users")
    );
}

#[test]
fn panel_refreshes_after_new_entry() {
    let db = memory_db();
    let engine = connected(
        FakeScript {
            rows: 1,
            ..Default::default()
        },
        db,
    );
    engine.search_history(HistoryChannel::Panel, query(""));
    assert!(wait_until(TIMEOUT, || view_entries(
        &engine,
        HistoryChannel::Panel
    )
    .is_some_and(|e| e.is_empty())));

    let q = engine.execute("c", "SELECT 1".into(), 500);
    settle(&engine, q);
    assert!(
        wait_until(Duration::from_secs(1), || view_entries(
            &engine,
            HistoryChannel::Panel
        )
        .is_some_and(|e| sqls(&e) == ["SELECT 1"])),
        "panel: {:?}",
        engine.history(HistoryChannel::Panel)
    );
}

#[test]
fn history_trimmed_in_the_background_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");
    let db = Db::open_with_keep(&path, 5).unwrap();
    for i in 0..8 {
        db.add_history(&HistoryEntry {
            id: 0,
            conn_id: "c".into(),
            database: None,
            sql: format!("q{i}"),
            started_at_ms: i,
            duration_ms: 1,
            outcome: HistoryOutcome::Rows(1),
        })
        .unwrap();
    }
    let db = Arc::new(db);
    // The engine must outlive the trim it starts.
    let _engine = engine_on(FakeScript::default(), db.clone());
    let kept = wait_stored(&db, 5);
    assert_eq!(sqls(&kept), ["q7", "q6", "q5", "q4", "q3"]);
}

#[test]
fn snippets_save_delete_and_persist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");

    let engine = engine_on(FakeScript::default(), Arc::new(Db::open(&path).unwrap()));
    assert!(engine.snippets().is_empty());
    engine.save_snippet(snippet("1", "reports", "alpha"));
    engine.save_snippet(snippet("2", "", "zed"));
    // Memory updates at once, sorted by (folder, name).
    assert_eq!(snippet_ids(&engine), ["2", "1"]);
    // Saving an existing id replaces it.
    engine.save_snippet(snippet("1", "reports", "beta"));
    assert_eq!(snippet_ids(&engine), ["2", "1"]);
    assert_eq!(engine.snippets()[1].name, "beta");
    // Dropping the engine flushes its pending writes.
    drop(engine);

    let engine = engine_on(FakeScript::default(), Arc::new(Db::open(&path).unwrap()));
    assert!(
        wait_until(TIMEOUT, || snippet_ids(&engine) == ["2", "1"]),
        "snippets after restart: {:?}",
        engine.snippets()
    );
    assert_eq!(engine.snippets()[1].name, "beta");
    engine.delete_snippet("2");
    assert_eq!(snippet_ids(&engine), ["1"]);
    drop(engine);

    let db = Db::open(&path).unwrap();
    let ids: Vec<String> = db
        .list_snippets()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, ["1"]);
}

#[test]
fn notice_on_write_failure() {
    let dir = tempfile::tempdir().unwrap();
    let db = read_only_db(&dir.path().join("sqlr.db"));
    let engine = connected(
        FakeScript {
            rows: 1,
            ..Default::default()
        },
        db,
    );
    // The connect's recent-connections write fails first.
    let mut early = Vec::new();
    assert!(wait_until(TIMEOUT, || {
        early.extend(engine.take_notices());
        !early.is_empty()
    }));
    assert_eq!(early.len(), 1, "notices: {early:?}");
    assert!(
        early[0].contains("Could not save recent connections"),
        "{early:?}"
    );
    assert!(engine.take_notices().is_empty());
    let q = engine.execute("c", "SELECT 1".into(), 500);
    settle(&engine, q);

    let mut notices = Vec::new();
    assert!(wait_until(TIMEOUT, || {
        notices.extend(engine.take_notices());
        !notices.is_empty()
    }));
    assert_eq!(notices.len(), 1, "notices: {notices:?}");
    assert!(
        notices[0].contains("Could not save history"),
        "notice: {}",
        notices[0]
    );
    // Taking notices drains them.
    assert!(engine.take_notices().is_empty());
}

#[test]
fn connect_touches_recent() {
    let db = memory_db();
    db.touch_recent("old", 5).unwrap();
    let engine = engine_on(FakeScript::default(), db.clone());
    // The stored list is loaded at startup.
    assert!(
        wait_until(TIMEOUT, || engine.recent().as_slice()
            == [("old".to_string(), 5)]),
        "recent: {:?}",
        engine.recent()
    );

    let before = now_ms();
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(
        wait_until(TIMEOUT, || engine
            .recent()
            .first()
            .is_some_and(|(id, at)| id == "c" && *at >= before)),
        "recent: {:?}",
        engine.recent()
    );
    assert_eq!(engine.recent().len(), 2);
    // Written behind to `sqlr.db`.
    assert!(wait_until(TIMEOUT, || db
        .recent(8)
        .unwrap()
        .first()
        .is_some_and(|(id, _)| id == "c")));

    // A failed connect and a transient session (the dialog's test) are not recorded.
    engine.connect(config("test:1", "fake"), Secrets::default(), dev());
    engine.connect(config("gone", "missing-driver"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("test:1"),
        Some(SessionStatus::Connected(_))
    )));
    std::thread::sleep(Duration::from_millis(50));
    let ids: Vec<String> = engine.recent().iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(ids, ["c", "old"]);

    engine.forget_recent("old");
    assert_eq!(engine.recent().len(), 1);
    assert!(wait_until(TIMEOUT, || db.recent(8).unwrap().len() == 1));
}

#[test]
fn clear_history_per_connection() {
    let db = memory_db();
    let engine = engine_on(FakeScript::default(), db.clone());
    for (conn, sql, at) in [("a", "qa1", 1), ("b", "qb", 2), ("a", "qa2", 3)] {
        db.add_history(&HistoryEntry {
            id: 0,
            conn_id: conn.into(),
            database: None,
            sql: sql.into(),
            started_at_ms: at,
            duration_ms: 1,
            outcome: HistoryOutcome::Rows(1),
        })
        .unwrap();
    }
    engine.clear_history(Some("a"));
    assert!(
        wait_until(TIMEOUT, || sqls(&stored(&db)) == ["qb"]),
        "{:?}",
        stored(&db)
    );
    engine.clear_history(None);
    assert!(
        wait_until(TIMEOUT, || stored(&db).is_empty()),
        "{:?}",
        stored(&db)
    );
}
