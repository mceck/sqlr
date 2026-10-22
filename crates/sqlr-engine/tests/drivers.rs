//! Installed sidecar drivers join the engine without a restart.

use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use sqlr_core::{ConnectionConfig, ConnectionProfile, Secrets};
use sqlr_engine::{DriverJobState, DriverRegistry, Engine, IndexState, QueryStatus, SessionStatus};
use sqlr_registry::{Registry, platform_key, sha256_hex};

#[allow(dead_code)]
mod support;
use support::{TIMEOUT, no_repaint, wait_until};

fn echo_binary() -> PathBuf {
    static BUILT: OnceLock<()> = OnceLock::new();
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    BUILT.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let mut build = Command::new(cargo);
        build.args(["build", "-p", "sqlr-driver-sdk", "--example", "echo_driver"]);
        if profile_dir.file_name().is_some_and(|n| n == "release") {
            build.arg("--release");
        }
        assert!(build.status().unwrap().success());
    });
    profile_dir.join("examples").join("echo_driver")
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
    engine.configure_drivers(data.path(), "", 5);
    assert!(wait_until(TIMEOUT, || engine.drivers_loaded()));
    assert!(engine.drivers().get("echo").is_some());
    assert!(engine.manifests().iter().any(|m| m.name == "Echo"));
    assert_eq!(engine.installed_drivers()[0].active, "1.0.0");
    // Registered at startup, spawned only now, on the first connection.
    connect_echo(&engine);
}

/// A signed `file://` mirror holding `echo` at each of `versions` (the newest listed).
fn mirror(dir: &std::path::Path, versions: &[&str]) -> (String, String) {
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let sign = |data: &[u8]| {
        minisign::sign(
            Some(&pair.pk),
            &pair.sk,
            Cursor::new(data),
            Some("test"),
            None,
        )
        .unwrap()
        .into_string()
    };
    let version = versions.last().unwrap();
    let bytes = archive(version);
    let artifact = dir.join(format!("echo-{version}.tar.gz"));
    std::fs::write(&artifact, &bytes).unwrap();
    let index = serde_json::json!({ "format": 1, "drivers": [{
        "id": "echo", "name": "Echo", "version": version, "min_protocol": "1.0.0",
        "family": "relational", "description": "", "homepage": "", "license": "MIT",
        "artifacts": { platform_key(): {
            "url": format!("file://{}", artifact.display()),
            "sha256": sha256_hex(&bytes), "signature": sign(&bytes),
        }},
    }]})
    .to_string();
    std::fs::write(dir.join("index.json"), &index).unwrap();
    std::fs::write(dir.join("index.json.minisig"), sign(index.as_bytes())).unwrap();
    (
        format!("file://{}", dir.join("index.json").display()),
        pair.pk.to_box().unwrap().into_string(),
    )
}

#[test]
fn install_registers_without_restart() {
    let data = tempfile::tempdir().unwrap();
    let mirror_dir = tempfile::tempdir().unwrap();
    let (url, key) = mirror(mirror_dir.path(), &["1.0.0"]);
    let engine = engine();
    engine.configure_drivers_with_key(data.path(), &url, 5, &key);
    assert!(wait_until(TIMEOUT, || engine.drivers_loaded()));
    assert_eq!(
        engine.driver_index(),
        IndexState::Idle,
        "nothing fetched until asked"
    );
    assert!(engine.drivers().get("echo").is_none());

    engine.refresh_driver_index();
    assert!(wait_until(TIMEOUT, || matches!(
        engine.driver_index(),
        IndexState::Ready(_)
    )));
    engine.install_driver("echo");
    assert!(
        wait_until(Duration::from_secs(20), || {
            engine
                .driver_jobs()
                .iter()
                .any(|(id, s)| id == "echo" && *s == DriverJobState::Done)
        }),
        "{:?}",
        engine.driver_jobs()
    );
    assert!(
        engine.drivers().get("echo").is_some(),
        "registered with no restart"
    );
    connect_echo(&engine);

    // Removing it takes it out of the engine again.
    engine.remove_driver("echo", None);
    assert!(wait_until(TIMEOUT, || engine
        .drivers()
        .get("echo")
        .is_none()));
    assert!(engine.installed_drivers().is_empty());
}

#[test]
fn a_bad_index_fails_the_refresh_not_the_engine() {
    let data = tempfile::tempdir().unwrap();
    let engine = engine();
    engine.configure_drivers(data.path(), "file:///no/such/index.json", 5);
    engine.refresh_driver_index();
    assert!(wait_until(TIMEOUT, || matches!(
        engine.driver_index(),
        IndexState::Failed(_)
    )));
    engine.install_driver("echo");
    assert!(
        engine
            .driver_jobs()
            .iter()
            .any(|(_, s)| matches!(s, DriverJobState::Failed(_)))
    );
}

#[test]
fn manual_install_waits_for_confirmation() {
    let data = tempfile::tempdir().unwrap();
    let file = data.path().join("echo.tar.gz");
    std::fs::write(&file, archive("0.2.0")).unwrap();
    let engine = engine();
    engine.configure_drivers(data.path(), "", 5);
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
    engine.configure_drivers(data.path(), "", 5);
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
