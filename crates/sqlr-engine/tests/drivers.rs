//! Installed sidecar drivers join the engine without a restart.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sqlr_core::{ConnectionConfig, ConnectionProfile, Secrets};
use sqlr_engine::{
    DriverJobState, DriverRegistry, Engine, QueryStatus, ReleaseSource, SessionStatus,
};
use sqlr_registry::{Registry, platform_key};

use crate::support;
use support::{TIMEOUT, no_repaint, wait_until};

fn echo_binary() -> PathBuf {
    sqlr_testkit::workspace_binary("examples/echo_driver")
}

fn archive(version: &str) -> Vec<u8> {
    use std::io::Write;
    let mut tar = tar::Builder::new(Vec::new());
    let mut add = |path: &str, data: &[u8]| {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, data).unwrap();
    };
    add(
        "driver.toml",
        format!("id = \"echo\"\nversion = \"{version}\"\nexecutable = \"echo\"\n").as_bytes(),
    );
    add("echo", &std::fs::read(echo_binary()).unwrap());
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar.into_inner().unwrap()).unwrap();
    gz.finish().unwrap()
}

fn engine() -> Engine {
    Engine::new(DriverRegistry::default(), no_repaint())
}

fn connect_echo(engine: &Engine) {
    engine.connect(
        ConnectionConfig {
            id: "e".into(),
            name: "e".into(),
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
        wait_until(TIMEOUT * 3, || matches!(
            engine.session_status("e"),
            Some(SessionStatus::Connected(_))
        )),
        "{:?}",
        engine.session_status("e")
    );
    let q = engine.execute("e", "rows 7".into(), 100);
    assert!(wait_until(TIMEOUT, || {
        engine
            .result(q)
            .is_some_and(|r| r.read().status == QueryStatus::Ready { more: false })
    }));
    assert_eq!(engine.result(q).unwrap().read().rows.row_count(), 7);
}

#[test]
fn installed_sidecar_registered_at_startup() {
    let data = tempfile::tempdir().unwrap();
    // A driver installed by an earlier run.
    let file = data.path().join("echo.tar.gz");
    std::fs::write(&file, archive("1.0.0")).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let registry = Registry::new(data.path().to_path_buf(), "file:///none".into(), "unused");
    rt.block_on(async {
        let inspection = registry.install_manual(&file).await.unwrap();
        registry.confirm_manual(&inspection).await.unwrap();
    });

    let engine = engine();
    assert!(engine.drivers().get("echo").is_none());
    engine.configure_drivers(data.path(), 5);
    assert!(wait_until(TIMEOUT, || engine.drivers_loaded()));
    assert!(engine.drivers().get("echo").is_some());
    assert!(engine.manifests().iter().any(|m| m.name == "Echo"));
    assert_eq!(engine.installed_drivers()[0].active, "1.0.0");
    // Registered at startup, spawned only now, on the first connection.
    connect_echo(&engine);
}

#[test]
fn manual_install_waits_for_confirmation() {
    let data = tempfile::tempdir().unwrap();
    let file = data.path().join("echo.tar.gz");
    std::fs::write(&file, archive("0.2.0")).unwrap();
    let engine = engine();
    engine.configure_drivers(data.path(), 5);
    engine.inspect_manual_install(file);
    assert!(wait_until(TIMEOUT, || matches!(
        engine.manual_install(),
        sqlr_engine::ManualState::Review(_)
    )));
    assert!(
        engine.drivers().get("echo").is_none(),
        "inspecting installs nothing"
    );
    engine.confirm_manual_install();
    assert!(wait_until(Duration::from_secs(20), || engine
        .drivers()
        .get("echo")
        .is_some()));
    assert!(engine.installed_drivers()[0].manual);
}

/// Runs `f` on another thread and fails, instead of hanging the suite, if it takes too long.
fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(15))
        .unwrap_or_else(|_| panic!("{what} did not finish: the engine is stuck"))
}

#[test]
fn a_job_while_another_driver_stays_installed_does_not_deadlock() {
    let data = tempfile::tempdir().unwrap();
    let engine = Arc::new(engine());
    engine.configure_drivers(data.path(), 5);
    // Two versions of the same driver stay installed.
    for version in ["1.0.0", "1.1.0"] {
        let file = data.path().join(format!("echo-{version}.tar.gz"));
        std::fs::write(&file, archive(version)).unwrap();
        engine.inspect_manual_install(file);
        assert!(wait_until(TIMEOUT, || matches!(
            engine.manual_install(),
            sqlr_engine::ManualState::Review(_)
        )));
        engine.confirm_manual_install();
        assert!(wait_until(Duration::from_secs(20), || {
            engine
                .installed_drivers()
                .first()
                .is_some_and(|d| d.versions.iter().any(|v| v == version))
        }));
    }
    // A rollback ends a job while `echo` is registered: the hub must not lock itself twice.
    // The installs above left a finished job behind; the rollback must be its own.
    engine.clear_driver_job("echo");
    engine.activate_driver("echo", "1.0.0");
    let e = engine.clone();
    let jobs = within("the rollback", move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let jobs = e.driver_jobs();
            if jobs.iter().any(|(_, s)| *s == DriverJobState::Done)
                && e.installed_drivers()[0].active == "1.0.0"
            {
                return e.installed_drivers()[0].active.clone();
            }
            assert!(std::time::Instant::now() < deadline, "{jobs:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    assert_eq!(jobs, "1.0.0");
}

/// A local mirror of a release that publishes `echo` at `version` for this platform, listed in
/// its `SHA256SUMS.txt` with `sha256` (the archive's own checksum by default).
fn release_mirror(dir: &std::path::Path, version: &str, sha256: Option<&str>) -> ReleaseSource {
    let release = ReleaseSource {
        base_url: format!("file://{}", dir.display()),
        version: version.into(),
    };
    let bytes = archive(version);
    let name = release.asset_name("echo", &platform_key());
    let sha = sha256.map_or_else(|| sqlr_registry::sha256_hex(&bytes), str::to_string);
    std::fs::write(dir.join("SHA256SUMS.txt"), format!("{sha}  {name}\n")).unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
    release
}

#[test]
fn a_driver_downloads_from_the_release_and_registers() {
    let data = tempfile::tempdir().unwrap();
    let mirror = tempfile::tempdir().unwrap();
    let engine = engine();
    engine.configure_drivers(data.path(), 5);
    // By default, drivers come from this version's GitHub release.
    assert_eq!(
        engine.driver_release(),
        ReleaseSource::github(env!("CARGO_PKG_VERSION"))
    );
    engine.set_driver_release(release_mirror(mirror.path(), "1.3.0", None));
    engine.download_driver("echo");
    assert!(wait_until(Duration::from_secs(20), || engine
        .drivers()
        .get("echo")
        .is_some()));
    let installed = engine.installed_drivers();
    assert_eq!(installed[0].active, "1.3.0");
    assert!(!installed[0].manual);
    assert!(wait_until(TIMEOUT, || engine
        .driver_jobs()
        .contains(&("echo".to_string(), DriverJobState::Done))));
    connect_echo(&engine);
}

#[test]
fn a_tampered_release_download_fails_and_installs_nothing() {
    let data = tempfile::tempdir().unwrap();
    let mirror = tempfile::tempdir().unwrap();
    let engine = engine();
    engine.configure_drivers(data.path(), 5);
    let other = sqlr_registry::sha256_hex(b"another file");
    engine.set_driver_release(release_mirror(mirror.path(), "1.3.0", Some(&other)));
    engine.download_driver("echo");
    assert!(wait_until(TIMEOUT, || {
        engine.driver_jobs().iter().any(|(id, state)| {
            id == "echo" && matches!(state, DriverJobState::Failed(m) if m.contains("sha256"))
        })
    }));
    assert!(engine.drivers().get("echo").is_none());
    assert!(engine.installed_drivers().is_empty());
}
