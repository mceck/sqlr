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
    app_with_terminal_settings(dir, shell, "")
}

fn app_with_terminal_settings(dir: &std::path::Path, shell: &str, cwd: &str) -> SqlrApp {
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
        terminal_cwd: cwd.into(),
        agents_port: {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        },
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
        mcp_token_store: Some(Arc::new(sqlr_store::MemoryMcpTokenStore::with_token(
            "terminal-test-token",
        ))),
    };
    SqlrApp::new(deps, &egui::Context::default())
}

fn wait_for_terminal_canvas(harness: &mut Harness<'_, SqlrApp>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.step();
        let rect = harness.state().terminal_rect_for_tests();
        if rect.width() > 0.0 && rect.height() > 0.0 {
            return;
        }
        assert!(Instant::now() < deadline, "terminal did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
#[test]
fn the_default_terminal_starts_in_the_config_workspace_with_mcp_instructions() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_shell(
        dir.path(),
        "pwd -P > shell-cwd; test -f AGENTS.md && test -f .mcp.json && printf 'agents-ready\\n'; sleep 30",
    );
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
        if text.contains("agents-ready") {
            break;
        }
        assert!(Instant::now() < deadline, "terminal output: {text:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let workspace = dir.path().join("terminal");
    assert_eq!(
        std::fs::read_to_string(workspace.join("shell-cwd"))
            .unwrap()
            .trim(),
        workspace.canonicalize().unwrap().to_str().unwrap()
    );
    let guide = std::fs::read_to_string(workspace.join("AGENTS.md")).unwrap();
    for required in [
        "Copy MCP config",
        "get_guide",
        "config_list",
        "browse",
        "close_result",
    ] {
        assert!(guide.contains(required), "missing {required}: {guide}");
    }
    assert!(guide.contains(&dir.path().join("mcp.json").display().to_string()));
    assert!(!guide.contains("Bearer "));
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(workspace.join(".mcp.json")).unwrap())
            .unwrap();
    assert_eq!(config["mcpServers"], serde_json::json!({}));

    // Enabling MCP after opening the terminal refreshes the configuration in place.
    harness.state_mut().set_mcp_enabled_for_tests(true);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.step();
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(workspace.join(".mcp.json")).unwrap())
                .unwrap();
        if config["mcpServers"]["sqlr"]["headers"]["Authorization"] == "Bearer terminal-test-token"
        {
            assert!(
                config["mcpServers"]["sqlr"]["url"]
                    .as_str()
                    .unwrap()
                    .starts_with("http://127.0.0.1:")
            );
            let _ = rustls::crypto::ring::default_provider().install_default();
            let response = tokio::runtime::Runtime::new().unwrap().block_on(async {
                reqwest::Client::new()
                    .post(config["mcpServers"]["sqlr"]["url"].as_str().unwrap())
                    .header(
                        "Authorization",
                        config["mcpServers"]["sqlr"]["headers"]["Authorization"]
                            .as_str()
                            .unwrap(),
                    )
                    .header("Accept", "application/json, text/event-stream")
                    .json(&serde_json::json!({
                        "jsonrpc": "2.0", "id": 1, "method": "initialize",
                        "params": {
                            "protocolVersion": "2025-11-25", "capabilities": {},
                            "clientInfo": {"name": "terminal-test", "version": "1.0"}
                        }
                    }))
                    .send()
                    .await
                    .unwrap()
            });
            assert!(
                response.status().is_success(),
                "MCP config must authenticate"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "MCP configuration did not refresh"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&workspace).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(workspace.join(".mcp.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    // Reopening the panel keeps the same shell and its scratch files.
    std::fs::write(workspace.join("scratch.sql"), "select 1").unwrap();
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    harness.run_steps(2);
    assert!(workspace.join("scratch.sql").is_file());
    assert!(
        harness
            .state()
            .terminal_text_for_tests()
            .contains("agents-ready")
    );

    // Disabling MCP while the panel is hidden removes credentials from its config.
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    harness.state_mut().set_mcp_enabled_for_tests(false);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.step();
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(workspace.join(".mcp.json")).unwrap())
                .unwrap();
        if config["mcpServers"] == serde_json::json!({}) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "disabled MCP credentials remain in config"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn the_shell_starts_at_the_canvas_size() {
    // `stty size` runs while the child starts, before any later PTY resize could reach it.
    // A session spawned at the 80x24 default would report 80 columns and leave full-screen
    // agents drawing in the left half of the panel.
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_shell(dir.path(), "stty size > pty-size; sleep 30");
    let mut harness = Harness::builder()
        .with_size((1100.0, 760.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    wait_for_terminal_canvas(&mut harness);
    let path = dir.path().join("terminal").join("pty-size");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.step();
        // Shell redirection creates the file before stty writes its output.
        if let Ok(size) = std::fs::read_to_string(&path)
            && size.ends_with('\n')
        {
            let cols: u16 = size
                .split_whitespace()
                .nth(1)
                .expect("stty prints rows and columns")
                .parse()
                .unwrap();
            assert!(cols > 80, "the shell started at {size:?}");
            break;
        }
        assert!(Instant::now() < deadline, "the shell reported no size");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn the_workspace_links_claude_guidance_to_agents_md() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let mut harness = Harness::builder()
        .with_size((1100.0, 760.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    wait_for_terminal_canvas(&mut harness);
    let workspace = dir.path().join("terminal");
    let link = workspace.join("CLAUDE.md");
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        std::path::PathBuf::from("AGENTS.md")
    );
    assert_eq!(
        std::fs::read_to_string(&link).unwrap(),
        std::fs::read_to_string(workspace.join("AGENTS.md")).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn an_explicit_terminal_directory_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(cwd.path().join("AGENTS.md"), "user instructions").unwrap();
    let app = app_with_terminal_settings(
        dir.path(),
        "test -f AGENTS.md && printf 'custom-directory-ready\\n'; sleep 30",
        cwd.path().to_str().unwrap(),
    );
    let mut harness = Harness::builder()
        .with_size((1100.0, 760.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Backtick);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.step();
        if harness
            .state()
            .terminal_text_for_tests()
            .contains("custom-directory-ready")
        {
            break;
        }
        assert!(Instant::now() < deadline, "custom cwd was ignored");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read_to_string(cwd.path().join("AGENTS.md")).unwrap(),
        "user instructions"
    );
    assert!(!cwd.path().join(".mcp.json").exists());
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
    wait_for_terminal_canvas(&mut harness);
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
    wait_for_terminal_canvas(&mut harness);
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
    wait_for_terminal_canvas(&mut harness);
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
    wait_for_terminal_canvas(&mut harness);
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
    wait_for_terminal_canvas(&mut harness);
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
