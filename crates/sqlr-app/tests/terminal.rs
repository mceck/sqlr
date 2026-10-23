use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
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
    let settings = Settings {
        terminal_shell: "printf 'terminal-ready\\n'; sleep 30".into(),
        ..Settings::default()
    };
    let deps = AppDeps {
        engine,
        repo,
        settings,
        config_dir: dir.to_path_buf(),
        data_dir: None,
        startup_notice: None,
        storage_warning: None,
        mcp_token_store: None,
    };
    SqlrApp::new(deps, &egui::Context::default())
}

#[test]
fn terminal_toggles_starts_lazily_and_keeps_running_when_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let mut harness = Harness::builder()
        .with_size((1100.0, 760.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    assert!(!harness.state().terminal_visible_for_tests());

    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    harness.run_steps(2);
    assert!(harness.state().terminal_visible_for_tests());
    assert_eq!(
        harness.state().terminal_shell_for_tests(),
        "printf 'terminal-ready\\n'; sleep 30"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.step();
        let text = harness.state().terminal_text_for_tests();
        if text.contains("terminal-ready") {
            break;
        }
        assert!(Instant::now() < deadline, "terminal output: {text:?}");
        std::thread::sleep(Duration::from_millis(20));
    }

    // Hiding does not kill the shell: the screen is still there when it comes back.
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    harness.run_steps(2);
    assert!(!harness.state().terminal_visible_for_tests());
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    harness.run_steps(2);
    assert!(harness.state().terminal_visible_for_tests());
    let text = harness.state().terminal_text_for_tests();
    assert!(text.contains("terminal-ready"), "{text:?}");
}

#[test]
fn a_dialog_takes_the_keyboard_away_from_the_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let mut harness = Harness::builder()
        .with_size((1100.0, 760.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);

    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    harness.run_steps(2);
    assert!(harness.state().terminal_focused_for_tests());

    // The command palette asks for the keyboard: the terminal must stop reading keys in
    // the same frame, or typed text would land in both.
    harness.key_press_modifiers(
        egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
        egui::Key::P,
    );
    harness.run_steps(2);
    assert!(harness.state().palette_open());
    assert!(!harness.state().terminal_focused_for_tests());
}

#[test]
fn terminal_keys_do_not_move_the_ui_focus() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let mut harness = Harness::builder()
        .with_size((1100.0, 760.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);

    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    harness.run_steps(2);
    assert!(harness.state().terminal_focused_for_tests());

    // egui moves the focus on arrows, Tab and Escape unless the focused widget locks them;
    // in a terminal they belong to the shell and the focus must stay put.
    for key in [
        egui::Key::ArrowUp,
        egui::Key::ArrowDown,
        egui::Key::ArrowLeft,
        egui::Key::ArrowRight,
        egui::Key::Tab,
        egui::Key::Escape,
    ] {
        harness.key_press(key);
        harness.run_steps(2);
        assert!(
            harness.state().terminal_focused_for_tests(),
            "{key:?} moved the focus away from the terminal"
        );
    }
}
