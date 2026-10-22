//! Key-value state in the engine: SCAN cursors per pattern, value pages, edits and the
//! console (with its history in `sqlr.db`).

use std::sync::Arc;

use sqlr_core::{HistoryQuery, KeyType, KvEdit, KvPage, RespValue, Secrets, StatementClass};
use sqlr_engine::{DriverRegistry, Engine, SessionStatus};
use sqlr_store::Db;
use sqlr_testkit::{FakeKvDriver, FakeKvScript};

mod support;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn names(engine: &Engine, pattern: &str) -> Vec<String> {
    engine
        .kv_keys("r", 0, pattern)
        .map(|s| {
            s.keys
                .iter()
                .map(|k| String::from_utf8_lossy(&k.name).into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn idle(engine: &Engine, pattern: &str) -> bool {
    engine.kv_keys("r", 0, pattern).is_some_and(|s| !s.loading)
}

fn idle_in(engine: &Engine, db: u32, pattern: &str) -> bool {
    engine.kv_keys("r", db, pattern).is_some_and(|s| !s.loading)
}

fn script() -> FakeKvScript {
    let mut keys = Vec::new();
    for i in 0..10 {
        keys.push((
            format!("user:{i}").into_bytes(),
            KvPage::String {
                bytes: format!("u{i}").into_bytes(),
                total_len: 2,
            },
        ));
        keys.push((
            format!("order:{i}").into_bytes(),
            KvPage::String {
                bytes: b"o".to_vec(),
                total_len: 1,
            },
        ));
    }
    keys.push((
        b"big".to_vec(),
        KvPage::Hash {
            fields: (0..450)
                .map(|i| (format!("f{i:03}").into_bytes(), b"v".to_vec()))
                .collect(),
            cursor: 0,
        },
    ));
    FakeKvScript {
        keys,
        ..Default::default()
    }
}

fn connected(db: Option<Arc<Db>>) -> (Engine, Arc<FakeKvDriver>) {
    let fake = FakeKvDriver::new(script());
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = match db {
        Some(db) => Engine::with_db(drivers, no_repaint(), db),
        None => Engine::new(drivers, no_repaint()),
    };
    engine.connect(config("r", "fakekv"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("r"),
        Some(SessionStatus::Connected(_))
    )));
    (engine, fake)
}

#[test]
fn scan_state_per_pattern() {
    let (engine, fake) = connected(None);
    engine.set_kv_scan_count(3);
    assert!(engine.kv_keys("r", 0, "user:*").is_none());

    engine.kv_scan("r", 0, "user:*", false);
    assert!(wait_until(TIMEOUT, || idle(&engine, "user:*")));
    assert_eq!(names(&engine, "user:*"), ["user:0", "user:1", "user:2"]);
    assert!(engine.kv_keys("r", 0, "user:*").unwrap().more);

    // Another pattern has its own cursor.
    engine.kv_scan("r", 0, "order:*", false);
    assert!(wait_until(TIMEOUT, || idle(&engine, "order:*")));
    assert_eq!(names(&engine, "order:*"), ["order:0", "order:1", "order:2"]);

    // Back to the first pattern: "load more" continues from its own cursor.
    engine.kv_scan("r", 0, "user:*", true);
    assert!(wait_until(TIMEOUT, || idle(&engine, "user:*")
        && names(&engine, "user:*").len() == 6));
    assert_eq!(
        names(&engine, "user:*")[3..],
        ["user:3", "user:4", "user:5"]
    );
    assert_eq!(names(&engine, "order:*").len(), 3);
    let calls: Vec<(String, u64)> = fake
        .scans()
        .into_iter()
        .map(|(_, pattern, cursor)| (pattern, cursor))
        .collect();
    assert_eq!(
        calls,
        [
            ("user:*".to_string(), 0),
            ("order:*".to_string(), 0),
            ("user:*".to_string(), 3),
        ]
    );

    // A pattern without glob characters matches as a prefix; an empty one matches all.
    engine.kv_scan("r", 0, "order:1", false);
    assert!(wait_until(TIMEOUT, || idle(&engine, "order:1")));
    assert_eq!(fake.scans().last().unwrap().1, "order:1*");
    engine.set_kv_scan_count(500);
    engine.kv_scan("r", 0, "", false);
    assert!(wait_until(TIMEOUT, || idle(&engine, "")));
    assert_eq!(names(&engine, "").len(), 21);
    assert!(!engine.kv_keys("r", 0, "").unwrap().more);

    // A refresh starts the pattern over.
    engine.set_kv_scan_count(3);
    engine.kv_scan("r", 0, "user:*", false);
    assert!(wait_until(TIMEOUT, || idle(&engine, "user:*")
        && names(&engine, "user:*").len() == 3));

    // Deleting keys drops them from every loaded pattern.
    engine.kv_delete("r", 0, vec![b"user:0".to_vec(), b"order:1".to_vec()]);
    assert!(wait_until(TIMEOUT, || names(&engine, "user:*") == ["user:1", "user:2"]));
    assert!(!names(&engine, "").contains(&"order:1".to_string()));
    assert!(fake.key(0, b"user:0").is_none());

    // Disconnecting forgets the state.
    engine.disconnect("r");
    assert!(engine.kv_keys("r", 0, "user:*").is_none());
}

#[test]
fn value_pages_load_more_and_edits_reload() {
    let (engine, fake) = connected(None);
    engine.kv_open("r", 0, b"big");
    assert!(wait_until(TIMEOUT, || engine
        .kv_value("r", 0, b"big")
        .is_some_and(|v| !v.loading && v.page.is_some())));
    let v = engine.kv_value("r", 0, b"big").unwrap();
    assert_eq!(v.info.as_ref().unwrap().ty, KeyType::Hash);
    assert_eq!(v.page.as_ref().unwrap().len(), 200);
    engine.kv_load_more("r", 0, b"big");
    assert!(wait_until(TIMEOUT, || engine
        .kv_value("r", 0, b"big")
        .is_some_and(|v| !v.loading && v.page.unwrap().len() == 400)));

    engine.kv_edit(
        "r",
        0,
        b"big",
        KvEdit::HashSet(b"new".to_vec(), b"1".to_vec()),
    );
    // The value reloads from its first page after an edit.
    assert!(wait_until(TIMEOUT, || {
        fake.edits().len() == 1
            && engine
                .kv_value("r", 0, b"big")
                .is_some_and(|v| !v.loading && v.page.unwrap().len() == 200)
    }));

    // A rename moves the key.
    engine.kv_edit("r", 0, b"user:1", KvEdit::Rename(b"user:x".to_vec()));
    assert!(wait_until(TIMEOUT, || fake.key(0, b"user:x").is_some()));
    // A failed edit shows on the value.
    engine.kv_open("r", 0, b"user:2");
    engine.kv_edit("r", 0, b"user:2", KvEdit::Rename(b"user:x".to_vec()));
    assert!(wait_until(TIMEOUT, || engine
        .kv_value("r", 0, b"user:2")
        .is_some_and(|v| v.error.is_some())));

    // A key that is gone says so.
    engine.kv_open("r", 0, b"nope");
    assert!(wait_until(TIMEOUT, || engine
        .kv_value("r", 0, b"nope")
        .is_some_and(|v| !v.loading && v.missing)));
}

#[test]
fn console_runs_commands_and_multi_exec_blocks() {
    let (engine, _fake) = connected(None);
    assert_eq!(
        engine.kv_classify("r", "SET k v"),
        Some(StatementClass::Write)
    );
    assert_eq!(engine.kv_classify("r", "  "), None);
    engine.kv_command("r", 1, "SET k \"a b\"");
    engine.kv_command("r", 1, "GET k");
    assert!(wait_until(TIMEOUT, || engine
        .kv_console("r", 1)
        .output
        .len()
        == 2));
    let console = engine.kv_console("r", 1);
    assert_eq!(
        *console.output[0],
        ("SET k \"a b\"".to_string(), RespValue::Simple("OK".into()))
    );
    assert_eq!(console.output[1].1, RespValue::Bulk(b"a b".to_vec()));

    engine.kv_command("r", 1, "MULTI");
    engine.kv_command("r", 1, "SET n 1");
    engine.kv_command("r", 1, "GET n");
    assert!(wait_until(TIMEOUT, || engine
        .kv_console("r", 1)
        .output
        .len()
        == 5));
    let console = engine.kv_console("r", 1);
    assert_eq!(console.queued, Some(2));
    assert_eq!(console.output[3].1, RespValue::Simple("QUEUED".into()));
    engine.kv_command("r", 1, "EXEC");
    assert!(wait_until(TIMEOUT, || engine
        .kv_console("r", 1)
        .output
        .len()
        == 6));
    let console = engine.kv_console("r", 1);
    assert_eq!(console.queued, None);
    assert_eq!(
        console.output[5].1,
        RespValue::Array(vec![
            RespValue::Simple("OK".into()),
            RespValue::Bulk(b"1".to_vec())
        ])
    );

    // SELECT switches the console's database; a bad line is an error reply.
    engine.kv_command("r", 1, "SELECT 4");
    engine.kv_command("r", 1, "GET \"open");
    assert!(wait_until(TIMEOUT, || engine
        .kv_console("r", 1)
        .output
        .len()
        == 8));
    let console = engine.kv_console("r", 1);
    assert_eq!(console.db, 4);
    assert!(matches!(console.output[7].1, RespValue::Error(_)));
}

#[test]
fn console_history_is_persisted_without_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");
    {
        let db = Arc::new(Db::open(&path).unwrap());
        let (engine, _fake) = connected(Some(db));
        engine.kv_command("r", 1, "AUTH hunter2");
        engine.kv_command("r", 1, "PING");
        assert!(wait_until(TIMEOUT, || engine
            .kv_console("r", 1)
            .output
            .len()
            == 2));
        assert_eq!(engine.kv_console("r", 1).output[0].0, "AUTH ***");
    }
    let db = Arc::new(Db::open(&path).unwrap());
    let stored = db
        .search_history(&HistoryQuery {
            conn_id: Some("r".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(
        stored.iter().all(|e| !e.sql.contains("hunter2")),
        "{stored:?}"
    );
    let (engine, _fake) = connected(Some(db));
    // History comes back oldest first, for ↑ in the console.
    assert!(wait_until(TIMEOUT, || *engine.kv_console("r", 1).history
        == ["AUTH ***".to_string(), "PING".to_string()]));
}

#[test]
fn read_only_reaches_key_value_connections() {
    let fake = FakeKvDriver::new(script());
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    let read_only = sqlr_core::ConnectionProfile {
        read_only: true,
        ..dev()
    };
    engine.connect(config("r", "fakekv"), Secrets::default(), read_only);
    assert!(wait_until(TIMEOUT, || fake.read_only()));
    let key = sqlr_engine::SessionKey::meta("r");
    engine.set_read_only(&key, false);
    assert!(wait_until(TIMEOUT, || !fake.read_only()));
    engine.set_read_only(&key, true);
    assert!(wait_until(TIMEOUT, || fake.read_only()));
    // The driver refuses a write the console sends.
    engine.kv_command("r", 1, "SET k v");
    assert!(wait_until(TIMEOUT, || engine
        .kv_console("r", 1)
        .output
        .len()
        == 1));
    assert!(matches!(
        &engine.kv_console("r", 1).output[0].1,
        RespValue::Error(m) if m.contains("read-only")
    ));
}

#[test]
fn a_password_in_a_line_that_does_not_parse_never_shows_or_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");
    let lines = [
        "AUTH \"hunter2",
        "\"AUTH\" \"hunter2",
        "'auth' 'hunter2",
        "hello 3 auth u \"hunter2",
        "\"CONFIG\"x SET requirepass hunter2",
    ];
    {
        let db = Arc::new(Db::open(&path).unwrap());
        let (engine, _fake) = connected(Some(db));
        for line in lines {
            engine.kv_command("r", 1, line);
        }
        assert!(wait_until(TIMEOUT, || engine
            .kv_console("r", 1)
            .output
            .len()
            == lines.len()));
        let console = engine.kv_console("r", 1);
        for entry in console.output.iter() {
            assert_eq!(entry.0, "<unparsed> ***");
            assert!(matches!(entry.1, RespValue::Error(_)));
        }
        assert!(console.history.is_empty(), "{:?}", console.history);
    }
    let db = Db::open(&path).unwrap();
    let stored = db
        .search_history(&HistoryQuery {
            conn_id: Some("r".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(stored.is_empty(), "nothing is recorded: {stored:?}");
}

#[test]
fn each_tab_has_its_own_console_database() {
    let (engine, fake) = connected(None);
    engine.kv_set_console_db("r", 1, 0);
    engine.kv_set_console_db("r", 2, 3);
    engine.kv_command("r", 1, "SET a 1");
    engine.kv_command("r", 2, "SET b 2");
    assert!(wait_until(TIMEOUT, || fake.commands().len() == 2));
    let mut seen: Vec<(u32, Vec<u8>)> = fake
        .commands()
        .into_iter()
        .map(|(db, args)| (db, args[1].clone()))
        .collect();
    seen.sort();
    assert_eq!(seen, [(0, b"a".to_vec()), (3, b"b".to_vec())]);
    // SELECT in one console changes only that console.
    engine.kv_command("r", 1, "SELECT 5");
    assert!(wait_until(TIMEOUT, || engine.kv_console("r", 1).db == 5));
    assert_eq!(engine.kv_console("r", 2).db, 3);
    engine.kv_command("r", 2, "SET c 3");
    assert!(wait_until(TIMEOUT, || fake.commands().len() == 3));
    assert_eq!(fake.commands()[2].0, 3);
    // Each console has its own output.
    assert_eq!(engine.kv_console("r", 2).output.len(), 2);
    engine.kv_close_console("r", 2);
    assert!(engine.kv_console("r", 2).output.is_empty());
}

#[test]
fn a_blocking_console_command_does_not_hold_up_read_only() {
    let (engine, fake) = connected(None);
    fake.block_commands_matching("BLPOP");
    engine.kv_command("r", 1, "BLPOP q 0");
    assert!(wait_until(TIMEOUT, || engine.kv_console("r", 1).running));
    let key = sqlr_engine::SessionKey::meta("r");
    engine.set_read_only(&key, true);
    assert!(
        wait_until(TIMEOUT, || fake.read_only()),
        "the switch ran while BLPOP blocks"
    );
    fake.release_blocked();
}

#[test]
fn only_the_key_being_shown_keeps_its_value_in_memory() {
    let (engine, _fake) = connected(None);
    for key in [&b"user:1"[..], b"user:2", b"big"] {
        engine.kv_open("r", 0, key);
        assert!(wait_until(TIMEOUT, || engine
            .kv_value("r", 0, key)
            .is_some_and(|v| !v.loading && v.page.is_some())));
    }
    assert!(engine.kv_value("r", 0, b"user:1").is_none());
    assert!(engine.kv_value("r", 0, b"user:2").is_none());
    assert!(engine.kv_value("r", 0, b"big").is_some());
}

#[test]
fn scans_are_bounded_per_database_and_released_with_their_tab() {
    let (engine, _fake) = connected(None);
    for i in 0..20 {
        let pattern = format!("user:{i}*");
        engine.kv_scan("r", 0, &pattern, false);
        assert!(wait_until(TIMEOUT, || idle(&engine, &pattern)));
    }
    let kept = (0..20)
        .filter(|i| engine.kv_keys("r", 0, &format!("user:{i}*")).is_some())
        .count();
    assert!(kept <= 8, "{kept} patterns kept");
    // The newest are the ones kept.
    assert!(engine.kv_keys("r", 0, "user:19*").is_some());
    assert!(engine.kv_keys("r", 0, "user:0*").is_none());
    engine.kv_scan("r", 3, "x*", false);
    assert!(wait_until(TIMEOUT, || idle_in(&engine, 3, "x*")));
    engine.kv_release("r", 0);
    assert!(engine.kv_keys("r", 0, "user:19*").is_none());
    assert!(engine.kv_value("r", 0, b"big").is_none());
    assert!(
        engine.kv_keys("r", 3, "x*").is_some(),
        "another database stays"
    );
}

#[test]
fn a_rename_is_reported_once_and_forgotten_when_the_old_name_is_opened_again() {
    let (engine, fake) = connected(None);
    engine.kv_open("r", 0, b"user:1");
    assert!(wait_until(TIMEOUT, || engine
        .kv_value("r", 0, b"user:1")
        .is_some_and(|v| v.page.is_some())));
    engine.kv_edit("r", 0, b"user:1", KvEdit::Rename(b"user:x".to_vec()));
    assert!(wait_until(TIMEOUT, || engine
        .kv_value("r", 0, b"user:1")
        .is_some_and(
            |v| v.renamed_to.as_deref() == Some(&b"user:x"[..])
        )));
    assert!(fake.key(0, b"user:x").is_some());
    // The app opens the new name; the old state goes.
    engine.kv_open("r", 0, b"user:x");
    assert!(engine.kv_value("r", 0, b"user:1").is_none());
    // The old name opened again is just a missing key, not a redirect.
    engine.kv_open("r", 0, b"user:1");
    assert!(wait_until(TIMEOUT, || engine
        .kv_value("r", 0, b"user:1")
        .is_some_and(|v| !v.loading && v.missing)));
    assert_eq!(engine.kv_value("r", 0, b"user:1").unwrap().renamed_to, None);
}

#[test]
fn key_value_requests_are_jobs_of_the_general_job_system() {
    use sqlr_engine::{JobKind, JobOutcome};
    let (engine, _fake) = connected(None);
    let job_done = |id| {
        wait_until(TIMEOUT, || {
            engine.job(id).is_some_and(|j| j.outcome.is_some())
        })
    };
    let scan = engine.kv_scan("r", 0, "user:*", false).expect("a job");
    assert!(job_done(scan));
    let state = engine.job(scan).unwrap();
    assert_eq!(state.kind, JobKind::KvScan);
    assert_eq!(state.outcome, Some(JobOutcome::Done));

    let open = engine.kv_open("r", 0, b"big");
    assert!(job_done(open));
    assert_eq!(engine.job(open).unwrap().kind, JobKind::KvValue);

    // A failing edit ends the job `Failed`, and the value shows the error too.
    let edit = engine.kv_edit("r", 0, b"user:1", KvEdit::Rename(b"user:2".to_vec()));
    assert!(job_done(edit));
    assert_eq!(engine.job(edit).unwrap().kind, JobKind::KvEdit);
    assert!(matches!(
        engine.job(edit).unwrap().outcome,
        Some(JobOutcome::Failed(_))
    ));

    let command = engine.kv_command("r", 1, "PING").expect("a job");
    assert!(job_done(command));
    assert_eq!(engine.job(command).unwrap().kind, JobKind::KvConsole);
    // A console line that runs locally is no job.
    assert!(engine.kv_command("r", 1, "MULTI").is_none());
}

#[test]
fn only_the_last_finished_key_value_jobs_are_kept() {
    let (engine, _fake) = connected(None);
    let first = engine.kv_scan("r", 0, "user:*", false).unwrap();
    assert!(wait_until(TIMEOUT, || engine
        .job(first)
        .is_some_and(|j| j.outcome.is_some())));
    let mut last = first;
    for i in 0..100 {
        last = engine.kv_open("r", 0, format!("k{i}").as_bytes());
    }
    assert!(wait_until(TIMEOUT, || engine
        .job(last)
        .is_some_and(|j| j.outcome.is_some())));
    // Give the retirement of earlier jobs a moment.
    assert!(wait_until(TIMEOUT, || engine.job(first).is_none()));
}
