use std::sync::Arc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_core::{ConnectionConfig, Secrets};
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
use sqlr_testkit::FakeDriver;

fn app(dir: &std::path::Path) -> SqlrApp {
    let mut drivers = DriverRegistry::default();
    drivers.register(FakeDriver::new(Default::default()));
    let engine = Engine::new(drivers, Arc::new(|| {}));
    let secrets = Arc::new(MemorySecretStore::default());
    engine.set_secret_store(secrets.clone());
    let repo = ConnectionRepo::open(dir, Box::new(secrets)).unwrap();
    let cfg = ConnectionConfig {
        id: "c1".into(),
        name: "fake".into(),
        driver: "fake".into(),
        group: None,
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    };
    repo.save(&cfg, &Secrets::default()).unwrap();
    SqlrApp::new(
        AppDeps {
            engine,
            repo,
            settings: Settings::default(),
            config_dir: dir.to_path_buf(),
            data_dir: None,
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
        },
        &egui::Context::default(),
    )
}

#[test]
fn mcp_is_off_by_default_and_settings_explain_it() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let mut harness = Harness::builder()
        .with_size((1200.0, 820.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    assert!(harness.state().mcp_stopped_for_tests());
    assert!(!dir.path().join("mcp.json").exists());

    harness.state_mut().open_agents_settings_for_tests();
    harness.run_steps(3);
    assert!(harness.get_all_by_label("Agents").next().is_some());
    harness.get_by_label("Enable agent access (MCP)");
    harness.get_by_label("Stopped");
}

#[test]
fn the_connection_dialog_offers_agent_access() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let mut harness = Harness::builder()
        .with_size((1300.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    harness.get_by_label("New connection").click();
    harness.run_steps(3);
    harness.get_by_label("Agent access");
    for label in ["None", "Read", "Write"] {
        assert!(
            harness.get_all_by_label(label).next().is_some(),
            "{label} is offered"
        );
    }
}

#[test]
fn enabling_mcp_starts_the_listener_and_disabling_removes_the_discovery_file() {
    let dir = tempfile::tempdir().unwrap();
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut drivers = DriverRegistry::default();
    drivers.register(FakeDriver::new(Default::default()));
    let engine = Engine::new(drivers, Arc::new(|| {}));
    let secrets = Arc::new(MemorySecretStore::default());
    engine.set_secret_store(secrets.clone());
    let repo = ConnectionRepo::open(dir.path(), Box::new(secrets)).unwrap();
    let cfg = ConnectionConfig {
        id: "c1".into(),
        name: "fake".into(),
        driver: "fake".into(),
        group: None,
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    };
    repo.save(&cfg, &Secrets::default()).unwrap();
    let settings = Settings {
        agents_enabled: true,
        agents_port: port,
        ..Settings::default()
    };
    let token_store = Arc::new(sqlr_store::MemoryMcpTokenStore::with_token("ui-test-token"));
    let app = SqlrApp::new(
        AppDeps {
            engine,
            repo,
            settings,
            config_dir: dir.path().to_path_buf(),
            data_dir: Some(dir.path().to_path_buf()),
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: Some(token_store),
        },
        &egui::Context::default(),
    );
    let mut harness = Harness::builder()
        .with_size((1200.0, 820.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !harness.state().mcp_running_for_tests() {
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(std::time::Instant::now() < deadline, "MCP did not start");
    }
    let discovery = dir.path().join("mcp.json");
    let text = std::fs::read_to_string(&discovery).unwrap();
    assert!(text.contains(&format!("127.0.0.1:{port}")));
    assert!(!text.contains("ui-test-token"));
    harness.state_mut().set_mcp_enabled_for_tests(false);
    harness.run_steps(3);
    assert!(harness.state().mcp_stopped_for_tests());
    assert!(!discovery.exists());
}

#[test]
fn the_toolbar_toggle_switches_agent_access() {
    let dir = tempfile::tempdir().unwrap();
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut drivers = DriverRegistry::default();
    drivers.register(FakeDriver::new(Default::default()));
    let engine = Engine::new(drivers, Arc::new(|| {}));
    let secrets = Arc::new(MemorySecretStore::default());
    engine.set_secret_store(secrets.clone());
    let repo = ConnectionRepo::open(dir.path(), Box::new(secrets)).unwrap();
    let app = SqlrApp::new(
        AppDeps {
            engine,
            repo,
            settings: Settings {
                agents_port: port,
                ..Settings::default()
            },
            config_dir: dir.path().to_path_buf(),
            data_dir: Some(dir.path().to_path_buf()),
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: Some(Arc::new(sqlr_store::MemoryMcpTokenStore::with_token(
                "ui-test-token",
            ))),
        },
        &egui::Context::default(),
    );
    let mut harness = Harness::builder()
        .with_size((1200.0, 820.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    assert!(!harness.state().agents_enabled_for_tests());
    assert!(harness.state().mcp_stopped_for_tests());

    harness.get_by_label("Toggle agent access (MCP)").click();
    harness.run_steps(3);
    assert!(harness.state().agents_enabled_for_tests());

    harness.get_by_label("Toggle agent access (MCP)").click();
    harness.run_steps(3);
    assert!(!harness.state().agents_enabled_for_tests());
    assert!(harness.state().mcp_stopped_for_tests());
}

#[test]
fn the_approval_dialog_shows_the_statement_and_escape_denies() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let mut harness = Harness::builder()
        .with_size((1200.0, 820.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    let mut reply = harness.state_mut().queue_approval_for_tests();
    harness.run_steps(2);
    harness.get_by_label("Agent request");
    harness.get_by_label("drop table users");
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    let result = reply.try_recv().expect("the request is answered");
    assert_eq!(result.unwrap_err().code, "denied");
}
