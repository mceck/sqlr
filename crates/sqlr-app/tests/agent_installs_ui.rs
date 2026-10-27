//! Settings → Agents' agent list: states, Install with its one-time confirmation, Remove,
//! Copy config for Manual setup, and the rewrite of installations on token rotation.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sqlr_app::agent_install::AgentEnvironment;
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
use sqlr_testkit::FakeDriver;

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        Self {
            home,
            data,
            _dir: dir,
        }
    }

    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.home.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn app(&self, running: bool) -> SqlrApp {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let mut drivers = DriverRegistry::default();
        drivers.register(FakeDriver::new(Default::default()));
        let engine = Engine::new(drivers, Arc::new(|| {}));
        let secrets = Arc::new(MemorySecretStore::default());
        engine.set_secret_store(secrets.clone());
        let repo = ConnectionRepo::open(&self.data, Box::new(secrets)).unwrap();
        let mut app = SqlrApp::new(
            AppDeps {
                check_updates: false,
                engine,
                repo,
                settings: Settings {
                    agents_enabled: running,
                    agents_port: port,
                    ..Settings::default()
                },
                config_dir: self.data.clone(),
                data_dir: Some(self.data.clone()),
                startup_notice: None,
                storage_warning: None,
                mcp_token_store: Some(Arc::new(sqlr_store::MemoryMcpTokenStore::with_token(
                    "ui-test-token",
                ))),
                diagnostics: None,
                diagnostics_warning: None,
            },
            &egui::Context::default(),
        );
        app.set_agent_environment_for_tests(AgentEnvironment::in_home(&self.home));
        app
    }
}

fn harness(app: SqlrApp) -> Harness<'static, SqlrApp> {
    Harness::builder()
        .with_size((1200.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app)
}

/// Steps until `done` holds, sleeping between frames for the background workers.
fn wait(
    harness: &mut Harness<'static, SqlrApp>,
    what: &str,
    done: impl Fn(&Harness<SqlrApp>) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(harness) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.run_steps(2);
}

fn open_agents(harness: &mut Harness<'static, SqlrApp>) {
    harness.state_mut().open_agents_settings_for_tests();
    harness.run_steps(2);
}

/// Scrolls the Preferences page to the widget, then clicks it.
fn click(harness: &mut Harness<'static, SqlrApp>, label: &str) {
    harness.get_by_label(label).scroll_to_me();
    harness.run_steps(3);
    harness.get_by_label(label).click();
    harness.run_steps(2);
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn the_agent_list_shows_each_state_and_installs_after_one_confirmation() {
    let fixture = Fixture::new();
    // Cursor is set up with no configuration file yet; Gemini's file is broken.
    std::fs::create_dir_all(fixture.home.join(".cursor")).unwrap();
    let gemini = fixture.write(".gemini/settings.json", "{ \"mcpServers\": ");
    let mut harness = harness(fixture.app(true));
    wait(&mut harness, "the server", |h| {
        h.state().mcp_running_for_tests()
    });
    open_agents(&mut harness);
    wait(&mut harness, "the agent states", |h| {
        h.query_by_label("Cursor: Install").is_some()
    });

    for name in ["Claude Code", "Codex CLI", "VS Code", "OpenCode"] {
        assert!(
            harness
                .query_by_label(&format!("{name}: Not detected"))
                .is_some(),
            "{name} is not detected"
        );
    }
    harness.get_by_label("Cursor: Not installed");
    harness.get_by_label("Gemini CLI: Manual setup");
    harness.get_by_label("Gemini CLI: Copy config");
    harness.get_by_label("Other agent…");
    harness.get_by_label("Copy MCP config");

    // The first write to the file asks once, naming the file and the token.
    click(&mut harness, "Cursor: Install");
    let cursor = fixture.home.join(".cursor").join("mcp.json");
    harness.get_by_label_contains(&format!(
        "into {}. The file will hold the sqlr MCP token",
        cursor.display()
    ));
    assert!(
        !cursor.exists(),
        "nothing is written before the confirmation"
    );
    click(&mut harness, "Install");
    wait(&mut harness, "the install", |h| {
        h.query_by_label("Cursor: Remove").is_some()
    });
    harness.get_by_label("Cursor: Installed");
    let text = read(&cursor);
    assert!(text.contains("\"sqlr\""), "{text}");
    assert!(text.contains("Bearer ui-test-token"), "{text}");
    assert!(
        harness
            .state()
            .toast_texts()
            .iter()
            .any(|toast| toast == "Restart Cursor to load sqlr")
    );

    // Copy config gives the Manual setup agent its own format.
    click(&mut harness, "Gemini CLI: Copy config");
    assert!(
        harness
            .state()
            .toast_texts()
            .iter()
            .any(|toast| toast.contains("Gemini CLI configuration copied"))
    );
    assert_eq!(read(&gemini), "{ \"mcpServers\": ");

    // Remove takes sqlr's entry out and leaves the map.
    click(&mut harness, "Cursor: Remove");
    wait(&mut harness, "the removal", |h| {
        h.query_by_label("Cursor: Install").is_some()
    });
    let text = read(&cursor);
    assert!(!text.contains("sqlr"), "{text}");
    assert!(text.contains("mcpServers"), "{text}");
}

#[test]
fn install_update_and_copy_config_need_a_running_server() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.home.join(".cursor")).unwrap();
    fixture.write(".gemini/settings.json", "[");
    fixture.write(
        ".codex/config.toml",
        "[mcp_servers.sqlr]\nurl = \"http://127.0.0.1:1/mcp\"\nhttp_headers = { Authorization = \"Bearer old\" }\n",
    );
    let mut harness = harness(fixture.app(false));
    harness.run_steps(2);
    open_agents(&mut harness);
    wait(&mut harness, "the agent states", |h| {
        h.query_by_label("Cursor: Install").is_some()
    });
    assert!(
        harness
            .get_by_label("Cursor: Install")
            .accesskit_node()
            .is_disabled()
    );
    assert!(
        harness
            .get_by_label("Gemini CLI: Copy config")
            .accesskit_node()
            .is_disabled()
    );
    harness.get_by_label("Codex CLI: Outdated");
    assert!(
        harness
            .get_by_label("Codex CLI: Update")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn rotating_the_token_rewrites_recorded_installations() {
    let fixture = Fixture::new();
    let codex = fixture.write(".codex/config.toml", "# mine\nmodel = \"o3\"\n");
    let mut harness = harness(fixture.app(true));
    wait(&mut harness, "the server", |h| {
        h.state().mcp_running_for_tests()
    });
    open_agents(&mut harness);
    wait(&mut harness, "the agent states", |h| {
        h.query_by_label("Codex CLI: Install").is_some()
    });
    click(&mut harness, "Codex CLI: Install");
    click(&mut harness, "Install");
    wait(&mut harness, "the install", |h| {
        h.query_by_label("Codex CLI: Installed").is_some()
    });
    assert!(read(&codex).contains("Bearer ui-test-token"));
    assert!(
        read(&fixture.home.join(".codex/config.toml.sqlr-backup")) == "# mine\nmodel = \"o3\"\n"
    );

    click(&mut harness, "Rotate token");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.step();
        let text = read(&codex);
        if text.contains("Bearer ") && !text.contains("ui-test-token") {
            assert!(text.starts_with("# mine\nmodel = \"o3\"\n"), "{text}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the installation was not rewritten"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
