use std::sync::Arc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_core::{AgentAccess, ConnectionConfig, Secrets};
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
    let cfg = ConnectionConfig {
        id: "c2".into(),
        name: "work-db".into(),
        driver: "fake".into(),
        group: Some("Work".into()),
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    };
    repo.save(&cfg, &Secrets::default()).unwrap();
    SqlrApp::new(
        AppDeps {
            check_updates: false,
            engine,
            repo,
            settings: Settings::default(),
            config_dir: dir.to_path_buf(),
            data_dir: None,
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
            diagnostics: None,
            diagnostics_warning: None,
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
fn settings_lists_connections_grouped_by_folder_and_changes_their_access() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let mut harness = Harness::builder()
        .with_size((1200.0, 820.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    harness.state_mut().open_agents_settings_for_tests();
    harness.run_steps(3);

    harness.get_by_label("Connection access");
    harness.get_by_label("0 of 2 exposed");
    assert!(
        harness.get_all_by_label("Ungrouped").count() >= 1,
        "top-level connections get a heading when folders exist"
    );
    assert!(
        harness.get_all_by_label("Work").count() >= 2,
        "the folder is named by its header and the explorer"
    );
    harness.get_by_label("fake: None");
    harness.get_by_label("work-db: None");

    // The server settings come first, so the list is below the fold: scroll the page.
    harness.hover_at(egui::pos2(700.0, 500.0));
    for _ in 0..4 {
        harness.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -400.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(2);
    }
    harness.get_by_label("work-db: Write").click();
    harness.run_steps(3);
    assert_eq!(
        harness.state().agent_access_for_tests("c2"),
        AgentAccess::Write
    );
    assert_eq!(
        harness.state().agent_access_for_tests("c1"),
        AgentAccess::None
    );
    harness.get_by_label("1 of 2 exposed");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let text = std::fs::read_to_string(dir.path().join("connections.toml")).unwrap_or_default();
        if text.contains("agent_access = \"write\"") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "access was not saved");
        harness.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    harness.get_by_label("work-db: None").click();
    harness.run_steps(3);
    assert_eq!(
        harness.state().agent_access_for_tests("c2"),
        AgentAccess::None
    );
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
    harness.get_by_label("Advanced").click();
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
            check_updates: false,
            engine,
            repo,
            settings,
            config_dir: dir.path().to_path_buf(),
            data_dir: Some(dir.path().to_path_buf()),
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: Some(token_store),
            diagnostics: None,
            diagnostics_warning: None,
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
            check_updates: false,
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
            diagnostics: None,
            diagnostics_warning: None,
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

// ----- the MCP tools against the real app host -----

use sqlr_core::{Node, NodeKind, NodePath};
use sqlr_testkit::FakeScript;

/// An app whose Connection `c1` is exposed with `access` under `profile`, on a fake driver
/// with an `s/tables/orders` tree and a DDL-ready `orders` table.
fn agent_app(
    dir: &std::path::Path,
    access: AgentAccess,
    profile: Option<&str>,
) -> (Harness<'static, SqlrApp>, Arc<FakeDriver>) {
    let node = |parent: &NodePath, name: &str, kind, has_children| Node {
        path: parent.child(name),
        name: name.into(),
        kind,
        has_children,
        detail: None,
    };
    let root = NodePath::root();
    let s = node(&root, "s", NodeKind::Schema, true);
    let tables = node(&s.path, "tables", NodeKind::Folder, true);
    let orders = node(&tables.path, "orders", NodeKind::Table, false);
    let mut def = sqlr_core::TableDef::new(sqlr_core::TableRef {
        database: None,
        schema: None,
        name: "orders".into(),
    });
    def.columns = vec![sqlr_core::ColumnDef {
        name: "id".into(),
        type_name: "integer".into(),
        nullable: true,
        ..Default::default()
    }];
    let driver = FakeDriver::new(FakeScript {
        rows: 3,
        tree: vec![s, tables, orders],
        default_schemas: vec!["s".into()],
        editing: Some(sqlr_core::ObjectEditing {
            create: vec![sqlr_core::ObjectKind::Table],
            ..Default::default()
        }),
        objects: vec![sqlr_core::ObjectDef::Table(def)],
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(driver.clone());
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
        values: [("host".to_string(), "fake-host".to_string())].into(),
        profile: profile.map(str::to_string),
        save_secrets: true,
        agent_access: access,
    };
    repo.save(&cfg, &Secrets::default()).unwrap();
    let app = SqlrApp::new(
        AppDeps {
            check_updates: false,
            engine,
            repo,
            settings: Settings::default(),
            config_dir: dir.to_path_buf(),
            data_dir: None,
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
            diagnostics: None,
            diagnostics_warning: None,
        },
        &egui::Context::default(),
    );
    let mut harness = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    harness.run_steps(3);
    (harness, driver)
}

/// One MCP session calling tools on the app: each call runs on a runtime while the test
/// steps the app, which answers the host requests on its UI thread.
struct Agent {
    rt: tokio::runtime::Runtime,
    state: Arc<sqlr_mcp::state::ServerState>,
}

impl Agent {
    fn new(harness: &Harness<'static, SqlrApp>) -> Self {
        Self {
            rt: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
            state: harness.state().agent_server_for_tests(),
        }
    }

    fn start(
        &self,
        name: &'static str,
        args: serde_json::Value,
    ) -> tokio::task::JoinHandle<serde_json::Value> {
        let state = self.state.clone();
        self.rt.spawn(async move {
            let ctx = state.context("agent-test");
            sqlr_mcp::tools::call(&state, &ctx, name, args.as_object())
                .await
                .unwrap()
                .structured_content
                .unwrap()
        })
    }

    fn finish(
        &self,
        harness: &mut Harness<'static, SqlrApp>,
        handle: tokio::task::JoinHandle<serde_json::Value>,
    ) -> serde_json::Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !handle.is_finished() {
            harness.step();
            std::thread::sleep(std::time::Duration::from_millis(5));
            assert!(std::time::Instant::now() < deadline, "the tool call hung");
        }
        harness.run_steps(2);
        self.rt.block_on(handle).unwrap()
    }

    fn call(
        &self,
        harness: &mut Harness<'static, SqlrApp>,
        name: &'static str,
        args: serde_json::Value,
    ) -> serde_json::Value {
        let handle = self.start(name, args);
        self.finish(harness, handle)
    }
}

fn step_until_label(harness: &mut Harness<'static, SqlrApp>, label: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while harness.query_by_label(label).is_none() {
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(std::time::Instant::now() < deadline, "no {label}");
    }
}

#[test]
fn agent_tab_runs_wait_on_the_approval_dialog_and_run_once() {
    let dir = tempfile::tempdir().unwrap();
    let (mut harness, driver) = agent_app(dir.path(), AgentAccess::Write, Some("production"));
    let agent = Agent::new(&harness);
    let opened = agent.call(
        &mut harness,
        "open_query_tab",
        serde_json::json!({"connection_id":"c1","text":"delete from orders"}),
    );
    let tab = opened["tab_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("{opened}"));
    let runs = |driver: &FakeDriver| {
        driver
            .executed()
            .iter()
            .filter(|sql| sql.contains("delete from orders"))
            .count()
    };

    // Escape keeps the safe denial: nothing runs.
    let pending = agent.start(
        "run_query_tab",
        serde_json::json!({"tab_id":tab,"target":"script"}),
    );
    step_until_label(&mut harness, "Agent request");
    harness.get_by_label("delete from orders");
    harness.key_press(egui::Key::Escape);
    let denied = agent.finish(&mut harness, pending);
    assert_eq!(denied["error"]["code"], "denied", "{denied}");
    assert_eq!(runs(&driver), 0);

    // Allowing runs the statement once, in the tab, and answers its result ids.
    let pending = agent.start(
        "run_query_tab",
        serde_json::json!({"tab_id":tab,"target":"script"}),
    );
    step_until_label(&mut harness, "Agent request");
    harness.get_by_label("Allow").click();
    let allowed = agent.finish(&mut harness, pending);
    assert_eq!(allowed["tab_id"], tab, "{allowed}");
    assert_eq!(
        allowed["query_ids"].as_array().unwrap().len(),
        1,
        "{allowed}"
    );
    assert!(!allowed.to_string().contains("approval_required"));
    harness.run_steps(5);
    assert_eq!(runs(&driver), 1, "{:?}", driver.executed());
    assert!(
        harness.query_by_label("Agent request").is_none(),
        "no second dialog"
    );
}

#[test]
fn tab_results_transactions_and_selection_reach_the_agent() {
    let dir = tempfile::tempdir().unwrap();
    // Production commits manually: a read opens a transaction.
    let (mut harness, _driver) = agent_app(dir.path(), AgentAccess::Read, Some("production"));
    let agent = Agent::new(&harness);
    let opened = agent.call(
        &mut harness,
        "open_query_tab",
        serde_json::json!({"connection_id":"c1","text":"select n","title":"Proposal"}),
    );
    let tab = opened["tab_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("{opened}"));
    let (key, title, text, dirty) = harness.state_mut().active_tab_for_tests().unwrap();
    assert_eq!(
        (key, title.as_str(), text.as_str(), dirty),
        (tab, "Proposal", "select n", true)
    );

    let run = agent.call(
        &mut harness,
        "run_query_tab",
        serde_json::json!({"tab_id":tab,"target":{"statement":0}}),
    );
    assert_eq!(run["status"], "ready", "{run}");
    let id = run["query_ids"][0].as_str().unwrap().to_string();
    let page = agent.call(
        &mut harness,
        "get_query_result",
        serde_json::json!({"query_id": id}),
    );
    assert_eq!(page["kind"], "tabular", "{page}");
    assert_eq!(
        page["tabular"]["rows"],
        serde_json::json!([[0], [1], [2]]),
        "{page}"
    );
    let closed = agent.call(
        &mut harness,
        "close_result",
        serde_json::json!({"query_id": id}),
    );
    assert_eq!(closed["error"]["code"], "invalid", "{closed}");
    let again = agent.call(
        &mut harness,
        "get_query_result",
        serde_json::json!({"query_id": id, "offset": 2}),
    );
    assert_eq!(
        again["tabular"]["rows"],
        serde_json::json!([[2]]),
        "{again}"
    );

    // The grid selection: the bottom-most "2" is the result's last cell.
    harness.run_steps(5);
    harness
        .get_all_by_label("2")
        .max_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .unwrap()
        .click();
    harness.run_steps(3);
    let state = agent.call(
        &mut harness,
        "get_query_tab",
        serde_json::json!({"tab_id": tab}),
    );
    assert_eq!(
        state["last_run_query_ids"],
        serde_json::json!([id]),
        "{state}"
    );
    assert_eq!(state["transaction_open"], true, "{state}");
    assert_eq!(
        state["grid_selection"],
        serde_json::json!([{"column":"n","row":2,"value":2}]),
        "{state}"
    );
}

#[test]
fn show_in_app_opens_the_expected_views() {
    let dir = tempfile::tempdir().unwrap();
    let (mut harness, _driver) = agent_app(dir.path(), AgentAccess::Read, None);
    let agent = Agent::new(&harness);
    let path = serde_json::json!(["s", "tables", "orders"]);

    let explorer = agent.call(
        &mut harness,
        "show_in_app",
        serde_json::json!({"connection_id":"c1","view":"explorer","path":path}),
    );
    assert!(explorer.get("error").is_none(), "{explorer}");
    assert_eq!(
        harness.state().explorer_selection_for_tests(),
        Some((
            "c1".into(),
            vec!["s".into(), "tables".into(), "orders".into()]
        ))
    );

    let ddl = agent.call(
        &mut harness,
        "show_in_app",
        serde_json::json!({"connection_id":"c1","view":"ddl","path":path}),
    );
    let ddl_tab = ddl["tab_id"].as_u64().unwrap_or_else(|| panic!("{ddl}"));
    assert!(
        harness
            .state()
            .tab_titles()
            .contains(&"DDL · orders".to_string())
    );
    // Showing it again focuses the same tab.
    let again = agent.call(
        &mut harness,
        "show_in_app",
        serde_json::json!({"connection_id":"c1","view":"ddl","path":path}),
    );
    assert_eq!(again["tab_id"], ddl_tab, "{again}");

    let data = agent.call(
        &mut harness,
        "show_in_app",
        serde_json::json!({"connection_id":"c1","view":"table_data","path":path}),
    );
    let data_tab = data["tab_id"].as_u64().unwrap_or_else(|| panic!("{data}"));
    assert_ne!(data_tab, ddl_tab);
    assert_eq!(
        harness.state_mut().active_tab_for_tests().unwrap().0,
        data_tab
    );

    let er = agent.call(
        &mut harness,
        "show_in_app",
        serde_json::json!({"connection_id":"c1","view":"er_diagram","tables":["orders"]}),
    );
    assert!(er["tab_id"].is_u64(), "{er}");
    assert!(
        harness.state().tab_titles().contains(&"ER · s".to_string()),
        "{:?}",
        harness.state().tab_titles()
    );

    // The fake driver has no query plans.
    let explain = agent.call(
        &mut harness,
        "show_in_app",
        serde_json::json!({"connection_id":"c1","view":"explain","query":"select n"}),
    );
    assert_eq!(explain["error"]["code"], "unsupported", "{explain}");
}
