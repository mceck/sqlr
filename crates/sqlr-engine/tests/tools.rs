//! Native dump and restore jobs (spec §8.5), against a stand-in `sqlite3` script so the
//! runner is covered without a real database.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sqlr_core::{ConnectionProfile, DumpOptions, Secrets};
use sqlr_engine::{DriverRegistry, Engine, JobOutcome, JobState, SessionStatus};

const TIMEOUT: Duration = Duration::from_secs(10);

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn connected_sqlite(dir: &std::path::Path) -> (Engine, PathBuf) {
    let db = dir.join("app.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);")
        .unwrap();
    let mut drivers = DriverRegistry::default();
    drivers.register(sqlr_driver_sqlite::SqliteDriver::new());
    let engine = Engine::new(drivers, Arc::new(|| {}));
    let cfg = sqlr_core::ConnectionConfig {
        id: "s".into(),
        name: "sqlite".into(),
        driver: "sqlite".into(),
        group: None,
        color: None,
        values: [("path".to_string(), db.display().to_string())].into(),
        profile: None,
        save_secrets: true,
    };
    engine.connect(cfg, Secrets::default(), ConnectionProfile::development());
    assert!(
        wait_until(TIMEOUT, || matches!(
            engine.session_status("s"),
            Some(SessionStatus::Connected(_))
        )),
        "sqlite did not connect"
    );
    (engine, db)
}

#[cfg(unix)]
fn fake_tool(dir: &std::path::Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-sqlite3");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
fn wait_outcome(engine: &Engine, id: sqlr_engine::JobId) -> JobState {
    assert!(
        wait_until(TIMEOUT, || engine
            .job(id)
            .is_some_and(|j| j.outcome.is_some())),
        "job never finished"
    );
    engine.job(id).unwrap()
}

#[cfg(unix)]
#[test]
fn sqlite_dump_streams_stdout_to_the_target_and_logs_it() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _) = connected_sqlite(dir.path());
    let tool = fake_tool(
        dir.path(),
        "echo '-- fake dump'; echo 'CREATE TABLE t (x);'",
    );
    let out = dir.path().join("app.sql");
    let options = DumpOptions {
        executable: tool.display().to_string(),
        ..Default::default()
    };
    let id = engine.run_dump("s", "main", options, out.clone());
    let state = wait_outcome(&engine, id);
    assert_eq!(
        state.outcome,
        Some(JobOutcome::Dumped { path: out.clone() })
    );
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("-- fake dump"), "{text}");
    assert!(text.contains("CREATE TABLE t"), "{text}");
    assert!(
        state.log.iter().any(|l| l.starts_with("$ ")),
        "the command line is shown: {:?}",
        state.log
    );
}

#[cfg(unix)]
#[test]
fn a_cancelled_dump_leaves_no_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _) = connected_sqlite(dir.path());
    let tool = fake_tool(dir.path(), "sleep 10");
    let out = dir.path().join("app.sql");
    let options = DumpOptions {
        executable: tool.display().to_string(),
        ..Default::default()
    };
    let id = engine.run_dump("s", "main", options, out.clone());
    assert!(wait_until(TIMEOUT, || {
        engine.job(id).is_some_and(|j| !j.log.is_empty())
    }));
    engine.cancel_job(id);
    let state = wait_outcome(&engine, id);
    assert_eq!(state.outcome, Some(JobOutcome::Cancelled));
    assert!(!out.exists());
    let partial = dir.path().join(".app.sql.sqlr-partial");
    assert!(!partial.exists(), "partial file left behind: {partial:?}");
}

#[cfg(unix)]
#[test]
fn a_failing_dump_removes_the_partial_output() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _) = connected_sqlite(dir.path());
    let tool = fake_tool(dir.path(), "echo 'boom' >&2; exit 3");
    let out = dir.path().join("app.sql");
    let options = DumpOptions {
        executable: tool.display().to_string(),
        ..Default::default()
    };
    let id = engine.run_dump("s", "main", options, out.clone());
    let state = wait_outcome(&engine, id);
    assert!(matches!(state.outcome, Some(JobOutcome::Failed(_))));
    assert!(!out.exists());
    assert!(
        state.log.iter().any(|l| l.contains("boom")),
        "{:?}",
        state.log
    );
}

#[cfg(unix)]
#[test]
fn real_sqlite3_dump_and_binary_restore_round_trip() {
    if std::process::Command::new("sqlite3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped: sqlite3 is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (engine, _db) = connected_sqlite(dir.path());
    let backup = dir.path().join("backup.db");
    let options = DumpOptions {
        format: sqlr_core::DumpFormat::SqliteBinary,
        ..Default::default()
    };
    let id = engine.run_dump("s", "main", options, backup.clone());
    let state = wait_outcome(&engine, id);
    assert_eq!(
        state.outcome,
        Some(JobOutcome::Dumped {
            path: backup.clone()
        })
    );

    // Change the live database, then restore the copy over it.
    let db = dir.path().join("app.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("DROP TABLE t;")
        .unwrap();
    let id = engine.run_restore("s", "main", Default::default(), backup);
    let state = wait_outcome(&engine, id);
    assert!(
        matches!(state.outcome, Some(JobOutcome::Restored { .. })),
        "{:?}",
        state.outcome
    );
    let count: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[cfg(unix)]
#[test]
fn a_missing_executable_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _) = connected_sqlite(dir.path());
    let options = DumpOptions {
        executable: "/definitely/not/a/tool".into(),
        ..Default::default()
    };
    let id = engine.run_dump("s", "main", options, dir.path().join("app.sql"));
    let state = wait_outcome(&engine, id);
    let Some(JobOutcome::Failed(error)) = state.outcome else {
        panic!("expected a failure, got {:?}", state.outcome);
    };
    assert!(
        error.message.contains("does not exist"),
        "{}",
        error.message
    );
}

#[cfg(unix)]
#[test]
fn a_restore_of_a_missing_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _) = connected_sqlite(dir.path());
    let missing = dir.path().join("missing.sql");
    let id = engine.run_restore("s", "main", Default::default(), missing);
    let state = wait_outcome(&engine, id);
    let Some(JobOutcome::Failed(error)) = state.outcome else {
        panic!("expected a failure, got {:?}", state.outcome);
    };
    assert!(
        error.message.contains("does not exist"),
        "{}",
        error.message
    );
}

#[test]
fn a_dump_without_a_connection_fails_at_once() {
    let mut drivers = DriverRegistry::default();
    drivers.register(sqlr_driver_sqlite::SqliteDriver::new());
    let engine = Engine::new(drivers, Arc::new(|| {}));
    let dir = tempfile::tempdir().unwrap();
    let id = engine.run_dump(
        "missing",
        "main",
        DumpOptions::default(),
        dir.path().join("x.sql"),
    );
    assert!(wait_until(TIMEOUT, || {
        engine.job(id).is_some_and(|j| j.outcome.is_some())
    }));
    assert!(matches!(
        engine.job(id).unwrap().outcome,
        Some(JobOutcome::Failed(_))
    ));
}

#[cfg(unix)]
#[test]
fn cancel_kills_the_tools_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _) = connected_sqlite(dir.path());
    let pid_file = dir.path().join("child.pid");
    let tool = fake_tool(
        dir.path(),
        &format!("sleep 30 &\necho $! > {}\nwait", pid_file.display()),
    );
    let options = DumpOptions {
        executable: tool.display().to_string(),
        ..Default::default()
    };
    let id = engine.run_dump("s", "main", options, dir.path().join("app.sql"));
    assert!(wait_until(TIMEOUT, || {
        std::fs::read_to_string(&pid_file).is_ok_and(|p| p.trim().parse::<u32>().is_ok())
    }));
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    engine.cancel_job(id);
    let state = wait_outcome(&engine, id);
    assert_eq!(state.outcome, Some(JobOutcome::Cancelled));
    let alive = || {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    assert!(
        wait_until(Duration::from_secs(2), || !alive()),
        "the tool's child process survived the cancel"
    );
}

#[test]
fn sqlite_backup_and_restore_use_the_online_backup_api() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, db) = connected_sqlite(dir.path());
    let backup = dir.path().join("backup.db");
    // No sqlite3 is needed: the executable below does not exist.
    let options = DumpOptions {
        format: sqlr_core::DumpFormat::SqliteBinary,
        executable: "/nonexistent/sqlite3".into(),
        ..Default::default()
    };
    let id = engine.run_dump("s", "main", options, backup.clone());
    assert!(wait_until(TIMEOUT, || engine
        .job(id)
        .is_some_and(|j| j.outcome.is_some())));
    let state = engine.job(id).unwrap();
    assert_eq!(
        state.outcome,
        Some(JobOutcome::Dumped {
            path: backup.clone()
        }),
        "{:?}",
        state.log
    );
    assert!(
        state.total.is_some_and(|t| t > 0),
        "pages total reported: {:?}",
        state.total
    );
    // Change the live (still connected) database, then restore the copy over it.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("DROP TABLE t;")
        .unwrap();
    let options = sqlr_core::RestoreOptions {
        executable: "/nonexistent/sqlite3".into(),
        ..Default::default()
    };
    let id = engine.run_restore("s", "main", options, backup);
    assert!(wait_until(TIMEOUT, || engine
        .job(id)
        .is_some_and(|j| j.outcome.is_some())));
    let state = engine.job(id).unwrap();
    assert!(
        matches!(state.outcome, Some(JobOutcome::Restored { .. })),
        "{:?} {:?}",
        state.outcome,
        state.log
    );
    let count: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}
