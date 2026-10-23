use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_core::{ConnectionConfig, Secrets};
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
use sqlr_testkit::FakeDriver;

fn app(dir: &std::path::Path) -> SqlrApp {
    app_with_shell(dir, "printf 'terminal-ready\\n'; sleep 30")
}

fn app_with_shell(dir: &std::path::Path, shell: &str) -> SqlrApp {
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
        terminal_shell: shell.into(),
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

#[test]
fn the_kitty_keyboard_query_is_answered() {
    let dir = tempfile::tempdir().unwrap();
    // The application asks for the kitty keyboard protocol, then prints the reply as hex.
    let shell = "printf '\\033[?u'; stty raw -echo; head -c 5 | od -An -tx1; stty sane";
    let app = app_with_shell(dir.path(), shell);
    let mut harness = Harness::builder()
        .with_size((1100.0, 760.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.step();
        let text = harness.state().terminal_text_for_tests();
        // ESC [ ? 0 u : the current flags, and proof the reply reached the application.
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains("1b5b3f3075") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the kitty query got no answer: {text:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_pushed_kitty_mode_reaches_the_application() {
    let dir = tempfile::tempdir().unwrap();
    // The application enables the disambiguating flags and announces it, then reads one
    // key event and prints it as hex.
    let shell = "printf '\\033[>5u'; printf 'kitty-ready'; stty raw -echo; head -c 7 | od -An -tx1; stty sane";
    let app = app_with_shell(dir.path(), shell);
    let mut harness = Harness::builder()
        .with_size((1100.0, 760.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.step();
        if harness
            .state()
            .terminal_text_for_tests()
            .contains("kitty-ready")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the kitty mode was never announced"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(harness.state().terminal_focused_for_tests());
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::A);
    loop {
        harness.step();
        let text = harness.state().terminal_text_for_tests();
        // ESC [ 9 7 ; 5 u : Ctrl+A as CSI u rather than the legacy 0x01.
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains("1b5b39373b3575") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the application received no kitty key event: {text:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_wheel_reaches_an_application_that_enables_mouse_reports() {
    let dir = tempfile::tempdir().unwrap();
    // Read enough reports to also collect the wheel ones: egui smooths the wheel over a
    // few frames, so the first report after enabling the mode is usually a motion.
    let shell =
        "printf '\\033[?1003h\\033[?1006h'; stty raw -echo; head -c 64 | od -An -tx1; stty sane";
    let app = app_with_shell(dir.path(), shell);
    let mut harness = Harness::builder()
        .with_size((1100.0, 760.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    harness.run_steps(2);
    assert!(harness.state().terminal_visible_for_tests());

    let rect = harness.state().terminal_rect_for_tests();
    assert!(rect.width() > 0.0 && rect.height() > 0.0);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.event(egui::Event::PointerMoved(rect.center()));
        harness.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 40.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
        let text = harness.state().terminal_text_for_tests();
        // ESC [ < 64 ; : a wheel-up report.
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains("1b5b3c3634") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the application received no wheel report: {text:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn mouse_motion_reaches_an_application_that_enables_mouse_reports() {
    let dir = tempfile::tempdir().unwrap();
    // What opencode sets: any-motion tracking with SGR coordinates. The shell then prints
    // the first bytes of the first report it receives, as hex.
    let shell =
        "printf '\\033[?1003h\\033[?1006h'; stty raw -echo; head -c 8 | od -An -tx1; stty sane";
    let app = app_with_shell(dir.path(), shell);
    let mut harness = Harness::builder()
        .with_size((1100.0, 760.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    harness.run_steps(2);
    assert!(harness.state().terminal_visible_for_tests());

    let rect = harness.state().terminal_rect_for_tests();
    assert!(rect.width() > 0.0 && rect.height() > 0.0);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.event(egui::Event::PointerMoved(rect.center()));
        harness.step();
        let text = harness.state().terminal_text_for_tests();
        // ESC [ < 35 ; : a motion report with no button held. od's spacing differs
        // between platforms, so compare without whitespace.
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains("1b5b3c33353b") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the application received no mouse report: {text:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
