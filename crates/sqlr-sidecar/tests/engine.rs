//! The engine cannot tell a sidecar from an in-process driver (spec §6.2).

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use sqlr_core::{ConnectionConfig, ConnectionProfile, Secrets};
use sqlr_engine::{DriverRegistry, Engine, QueryId, QueryStatus, SessionStatus};
use sqlr_sidecar::{SidecarDriver, SidecarSpec};

fn echo_program() -> PathBuf {
    static BUILT: OnceLock<()> = OnceLock::new();
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    BUILT.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let mut build = Command::new(cargo);
        build.args(["build", "-p", "sqlr-driver-sdk", "--examples"]);
        if profile_dir.file_name().is_some_and(|n| n == "release") {
            build.arg("--release");
        }
        assert!(build.status().unwrap().success(), "building the examples");
    });
    profile_dir.join("examples").join("echo_driver")
}

fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn status(engine: &Engine, q: QueryId) -> QueryStatus {
    engine.result(q).unwrap().read().status.clone()
}

fn settled(engine: &Engine, q: QueryId) -> (QueryStatus, usize) {
    assert!(wait_until(|| status(engine, q) != QueryStatus::Running));
    let rows = engine.result(q).unwrap().read().rows.row_count();
    (status(engine, q), rows)
}

fn sidecar_engine() -> Engine {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (driver, _) = runtime
        .block_on(SidecarDriver::probe(SidecarSpec::new(
            "echo",
            "1.0.0",
            echo_program(),
        )))
        .unwrap();
    let mut drivers = DriverRegistry::default();
    drivers.register(Arc::new(driver));
    let engine = Engine::new(drivers, Arc::new(|| {}));
    engine.connect(
        ConnectionConfig {
            id: "c".into(),
            name: "c".into(),
            driver: "echo".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        },
        Secrets::default(),
        ConnectionProfile::development(),
    );
    assert!(
        wait_until(|| matches!(
            engine.session_status("c"),
            Some(SessionStatus::Connected(_))
        )),
        "{:?}",
        engine.session_status("c")
    );
    engine
}

#[test]
fn the_engine_runs_a_sidecar_like_a_bundled_driver() {
    let engine = sidecar_engine();
    let q = engine.execute("c", "rows 1500".into(), 500);
    assert_eq!(
        settled(&engine, q),
        (QueryStatus::Ready { more: true }, 500)
    );
    engine.fetch_all(q);
    assert!(wait_until(|| {
        engine.result(q).unwrap().read().rows.row_count() == 1500
            && status(&engine, q) == QueryStatus::Ready { more: false }
    }));

    let q = engine.execute("c", "affected 4".into(), 500);
    assert!(wait_until(|| status(&engine, q) != QueryStatus::Running));
    let q = engine.execute("c", "error nope".into(), 500);
    assert!(wait_until(|| matches!(
        status(&engine, q),
        QueryStatus::Failed(_)
    )));
}

#[test]
fn a_crashing_sidecar_fails_the_result_not_the_app() {
    let engine = sidecar_engine();
    let q = engine.execute("c", "crash".into(), 500);
    assert!(wait_until(|| matches!(
        status(&engine, q),
        QueryStatus::Failed(_)
    )));
    let QueryStatus::Failed(e) = status(&engine, q) else {
        unreachable!()
    };
    assert!(e.message.contains("echo"), "{}", e.message);
    // The engine is still alive and answers.
    assert!(engine.session_status("c").is_some());
}
