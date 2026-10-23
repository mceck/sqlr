use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_core::{
    CatalogColumn, CatalogRelation, ConnectionConfig, DriverError, ErrorKind, HistoryEntry,
    HistoryOutcome, Node, NodeKind, NodePath, RelationKind, SchemaCatalog, Secrets,
};
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, Db, MemorySecretStore, Settings};
use sqlr_testkit::{FakeDriver, FakeScript};
use sqlr_ui::theme::ThemePref;

const TIMEOUT: Duration = Duration::from_secs(5);

fn node(parent: &NodePath, name: &str, kind: NodeKind, has_children: bool) -> Node {
    Node {
        path: parent.child(name),
        name: name.into(),
        kind,
        has_children,
        detail: None,
    }
}

fn catalog() -> Vec<SchemaCatalog> {
    let column = |name: &str| CatalogColumn {
        name: name.into(),
        type_name: "int4".into(),
    };
    vec![SchemaCatalog {
        schema: "s".into(),
        relations: vec![CatalogRelation {
            name: "orders".into(),
            kind: RelationKind::Table,
            columns: vec![column("id"), column("total")],
        }],
    }]
}

fn deps_with(dir: &std::path::Path, script: FakeScript) -> (AppDeps, Arc<FakeDriver>) {
    deps_with_db(dir, script, None)
}

/// [`deps_with`], with the engine's library backed by `db` when given.
fn deps_with_db(
    dir: &std::path::Path,
    script: FakeScript,
    db: Option<Arc<Db>>,
) -> (AppDeps, Arc<FakeDriver>) {
    let root = NodePath::root();
    let s = node(&root, "s", NodeKind::Schema, true);
    let tables = node(&s.path, "tables", NodeKind::Folder, true);
    let orders = node(&tables.path, "orders", NodeKind::Table, true);
    let mut tree = vec![s, tables, orders];
    tree.extend(script.tree.iter().cloned());
    let driver = FakeDriver::new(FakeScript {
        rows: 1200,
        tree,
        catalog: if script.catalog.is_empty() {
            catalog()
        } else {
            script.catalog.clone()
        },
        default_schemas: vec!["s".into()],
        ..script
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(driver.clone());
    let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let engine = match db {
        Some(db) => Engine::with_db(drivers, repaint, db),
        None => Engine::new(drivers, repaint),
    };
    // The repo writes and the engine reads the same in-memory keychain, so a test can seed
    // stored secrets without touching the OS keychain.
    let secrets = Arc::new(MemorySecretStore::default());
    engine.set_secret_store(secrets.clone());

    let repo = ConnectionRepo::open(dir, Box::new(secrets)).unwrap();
    let cfg = ConnectionConfig {
        id: "c1".into(),
        name: "fake-conn".into(),
        driver: "fake".into(),
        group: None,
        color: None,
        values: [("host".to_string(), "fake-host".to_string())].into(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    };
    repo.save(&cfg, &Secrets::default()).unwrap();
    let deps = AppDeps {
        engine,
        repo,
        settings: Settings::default(),
        config_dir: dir.to_path_buf(),
        data_dir: None,
        startup_notice: None,
        storage_warning: None,
        mcp_token_store: None,
    };
    (deps, driver)
}

fn harness_with_driver(dir: &tempfile::TempDir) -> (Harness<'static, SqlrApp>, Arc<FakeDriver>) {
    harness_with_script(dir, FakeScript::default())
}

fn harness_with_script(
    dir: &tempfile::TempDir,
    script: FakeScript,
) -> (Harness<'static, SqlrApp>, Arc<FakeDriver>) {
    let (deps, driver) = deps_with(dir.path(), script);
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    (h, driver)
}

fn harness(dir: &tempfile::TempDir) -> Harness<'static, SqlrApp> {
    harness_with_driver(dir).0
}

/// A harness whose saved connection uses the connection profile `profile`.
fn harness_with_profile(
    dir: &tempfile::TempDir,
    profile: &str,
) -> (Harness<'static, SqlrApp>, Arc<FakeDriver>) {
    let (deps, driver) = deps_with(dir.path(), FakeScript::default());
    let mut cfg = deps.repo.list().unwrap().remove(0);
    cfg.profile = Some(profile.into());
    deps.repo.save(&cfg, &Secrets::default()).unwrap();
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    (h, driver)
}

/// Steps the harness (the engine works on its own thread) until `done` holds.
fn step_until(
    h: &mut Harness<'static, SqlrApp>,
    what: &str,
    done: impl Fn(&Harness<'static, SqlrApp>) -> bool,
) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        h.step();
        if done(h) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_label(h: &mut Harness<'static, SqlrApp>, label: &str) {
    step_until(h, label, |h| h.query_by_label(label).is_some());
}

fn double_click(h: &mut Harness<'static, SqlrApp>, label: &str) {
    // An earlier click less than 0.6 s (virtual) ago would turn this into a triple click.
    h.run_steps(100);
    let pos = h.get_by_label(label).rect().center();
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        h.step();
    }
}

/// Drags `from`'s row onto `to` and releases there.
fn drag_onto(h: &mut Harness<'static, SqlrApp>, from: &str, to: &str) {
    h.run_steps(100);
    let start = h.get_by_label(from).rect().center();
    let end = h.get_by_label(to).rect().center();
    h.event(egui::Event::PointerMoved(start));
    h.step();
    h.event(egui::Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    for i in 1..=12 {
        let t = i as f32 / 12.0;
        let pos = egui::pos2(
            start.x + (end.x - start.x) * t,
            start.y + (end.y - start.y) * t,
        );
        h.event(egui::Event::PointerMoved(pos));
        h.step();
    }
    h.event(egui::Event::PointerButton {
        pos: end,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

fn expand_to_orders(h: &mut Harness<'static, SqlrApp>) {
    h.get_by_label("Expand fake-conn").click();
    wait_for_label(h, "Expand s");
    h.get_by_label("Expand s").click();
    wait_for_label(h, "Expand tables");
    h.get_by_label("Expand tables").click();
    wait_for_label(h, "orders");
}

/// Opens `label`'s context menu and clicks its `item`.
fn context_menu(h: &mut Harness<'static, SqlrApp>, label: &str, item: &str) {
    h.run_steps(100);
    // The explorer row: the leftmost node of that name (the toolbar chip and the editor's
    // breadcrumb name the tab's connection too).
    h.get_all_by_label(label)
        .min_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
        .unwrap_or_else(|| panic!("no node labeled {label}"))
        .click_secondary();
    wait_for_label(h, item);
    h.get_by_label(item).click();
    h.run_steps(2);
}

fn open_orders(h: &mut Harness<'static, SqlrApp>) {
    expand_to_orders(h);
    double_click(h, "orders");
    step_until(h, "the first page", |h| {
        h.query_by_label_contains("500 rows (more available)")
            .is_some()
    });
}

#[test]
fn shows_saved_connection() {
    let dir = tempfile::tempdir().unwrap();
    let h = harness(&dir);
    h.get_by_label("fake-conn");
}

#[test]
fn database_menu_offers_native_dump_and_restore() {
    let dir = tempfile::tempdir().unwrap();
    let database = node(&NodePath::root(), "app", NodeKind::Database, true);
    let (mut h, _) = harness_with_script(
        &dir,
        FakeScript {
            tree: vec![database],
            dump: true,
            ..Default::default()
        },
    );
    h.get_by_label("Expand fake-conn").click();
    wait_for_label(&mut h, "app");
    h.get_by_label("app").click_secondary();
    wait_for_label(&mut h, "Dump database…");
    wait_for_label(&mut h, "Restore database…");
}

#[test]
fn explorer_copies_names_and_connection_urls() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    expand_to_orders(&mut h);
    context_menu(&mut h, "orders", "Copy name");
    wait_for_label(&mut h, "Copied orders");
    context_menu(&mut h, "fake-conn", "Copy connection URL");
    wait_for_label(&mut h, "fake-conn has no connection URL");
    context_menu(&mut h, "fake-conn", "New editor tab");
    h.run_steps(5);
    assert!(!h.state().tab_titles().is_empty());
}

#[test]
fn connection_menu_duplicates_it_with_its_stored_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let script = FakeScript {
        password: Some("hunter2".into()),
        ..FakeScript::default()
    };
    let (deps, _) = deps_with(dir.path(), script);
    let cfg = deps.repo.list().unwrap().remove(0);
    let secrets = Secrets([("password".to_string(), "hunter2".to_string())].into());
    deps.repo.save(&cfg, &secrets).unwrap();
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);

    // The store thread saves the copy.
    context_menu(&mut h, "fake-conn", "Duplicate");
    wait_for_label(&mut h, "fake-conn copy");
    // A second duplicate takes the next free name.
    context_menu(&mut h, "fake-conn", "Duplicate");
    wait_for_label(&mut h, "fake-conn copy 2");

    // The copy kept the stored password: it connects without asking for one.
    h.get_by_label("Expand fake-conn copy").click();
    wait_for_label(&mut h, "connected");
    assert_eq!(h.get_all_by_label("connected").count(), 1);
}

#[test]
fn open_table_runs_query_and_shows_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_driver(&dir);
    open_orders(&mut h);
    // Paging bounds what is fetched, so the generated SQL has no LIMIT.
    assert_eq!(driver.executed(), vec!["SELECT * FROM orders".to_string()]);
}

#[test]
fn fetch_all_completes() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    open_orders(&mut h);
    h.get_by_label("Fetch all").click();
    step_until(&mut h, "all rows", |h| {
        h.query_by_label_contains("1200 rows").is_some()
            && h.query_by_label_contains("more available").is_none()
    });
}

#[test]
fn new_connection_dialog_validates() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    assert!(h.query_by_label("Save").is_none());
    // The explorer's "+" button carries the same label as the menu item.
    let plus = h.get_by_label("New connection").accesskit_node().id();
    h.get_by_label("File").click();
    h.run_steps(2);
    h.get_all_by_label("New connection")
        .find(|n| n.accesskit_node().id() != plus)
        .expect("File menu has a New connection item")
        .click();
    h.run_steps(2);
    h.get_by_label("Name is required");
    assert!(h.get_by_label("Save").accesskit_node().is_disabled());
}

#[test]
fn refresh_reloads_expanded_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    expand_to_orders(&mut h);
    h.run_steps(100);
    h.get_by_label("s").click();
    h.run_steps(2);
    h.get_by_label("Refresh").click();
    h.step();
    assert!(
        h.query_by_label("orders").is_none(),
        "the refresh dropped s's subtree"
    );
    wait_for_label(&mut h, "Collapse tables");
    wait_for_label(&mut h, "orders");
}

#[test]
fn reconnect_reloads_expanded_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_driver(&dir);
    expand_to_orders(&mut h);
    let before = driver.connects().len();
    h.get_by_label("Reconnect").click();
    step_until(&mut h, "the reconnecting session", |_| {
        driver.connects().len() > before
    });
    wait_for_label(&mut h, "orders");
    h.get_by_label("connected");
}

#[test]
fn disconnecting_collapses_the_connection() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    expand_to_orders(&mut h);
    context_menu(&mut h, "fake-conn", "Disconnect");
    wait_for_label(&mut h, "disconnected");
    // The disconnection collapses the tree, so one click reconnects and expands it.
    h.get_by_label("Expand fake-conn").click();
    wait_for_label(&mut h, "s");
    h.get_by_label("connected");
}

/// Clicks into the active editor and moves the caret to the end of its text.
fn focus_editor_end(h: &mut Harness<'static, SqlrApp>) {
    h.run_steps(100);
    h.get_by_label("SQL editor").click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::ArrowDown);
    h.run_steps(2);
}

fn type_text(h: &mut Harness<'static, SqlrApp>, text: &str) {
    h.event(egui::Event::Text(text.into()));
    h.run_steps(2);
}

fn press(h: &mut Harness<'static, SqlrApp>, modifiers: Modifiers, key: Key) {
    h.key_press_modifiers(modifiers, key);
    h.run_steps(2);
}

/// Replaces the active editor's text.
fn replace_text(h: &mut Harness<'static, SqlrApp>, text: &str) {
    focus_editor_end(h);
    press(h, Modifiers::COMMAND, Key::A);
    type_text(h, text);
}

#[test]
fn run_statement_under_caret() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_driver(&dir);
    open_orders(&mut h);
    replace_text(&mut h, "select 1;\nselect 2;");
    press(&mut h, Modifiers::COMMAND, Key::Enter);
    step_until(&mut h, "the statement to run", |_| {
        driver.executed().len() == 2
    });
    assert_eq!(
        driver.executed().last().map(String::as_str),
        Some("select 2")
    );
}

#[test]
fn shift_cmd_enter_runs_script() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_driver(&dir);
    open_orders(&mut h);
    let before = driver.executed().len();
    replace_text(&mut h, "select 1; select 2;");
    press(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter);
    step_until(&mut h, "the script to finish", |h| {
        h.query_by_label("Result 2").is_some()
    });
    h.run_steps(20);
    let executed = driver.executed();
    assert_eq!(executed.len(), before + 2, "{executed:?}");
    assert_eq!(executed[before..], ["select 1", "select 2"]);
    h.get_by_label("Result 1");
    h.get_by_label("Output");
}

#[test]
fn completion_uses_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    open_orders(&mut h);
    focus_editor_end(&mut h);
    type_text(&mut h, " WHERE orders.");
    // A popup row's label is its badge, name and type: "col  total  int4".
    step_until(&mut h, "the column completion", |h| {
        h.query_by_label_contains("total").is_some()
    });
}

#[test]
fn history_panel_lists_runs() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    open_orders(&mut h);
    h.get_by_label("View").click();
    h.run_steps(2);
    h.get_by_label("History").click();
    step_until(&mut h, "the history entry", |h| {
        h.query_by_label_contains("SELECT * FROM orders").is_some()
    });
}

#[test]
fn palette_opens_object() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_driver(&dir);
    open_orders(&mut h);
    let before = driver.executed().len();
    press(&mut h, Modifiers::COMMAND, Key::P);
    wait_for_label(&mut h, "s.orders");
    type_text(&mut h, "ord");
    press(&mut h, Modifiers::NONE, Key::Enter);
    step_until(&mut h, "the palette's query to run", |_| {
        driver.executed().len() == before + 1
    });
    assert_eq!(h.state().tab_titles(), ["Query 1", "orders", "orders"]);
}

#[test]
fn command_palette_runs_command() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    press(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::P);
    type_text(&mut h, "new query");
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().tab_titles(), ["Query 1", "Query 2"]);
}

#[test]
fn snippet_save_and_insert() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    replace_text(&mut h, "select 42");
    h.get_by_label("Query").click();
    h.run_steps(2);
    h.get_by_label("Save as snippet…").click();
    h.run_steps(3);
    type_text(&mut h, "mine");
    h.get_by_label("Save").click();
    h.run_steps(2);
    h.get_by_label("Snippets").click();
    wait_for_label(&mut h, "mine");
    double_click(&mut h, "mine");
    h.run_steps(2);
    let text = h.state_mut().active_text().unwrap();
    assert_eq!(text.matches("select 42").count(), 2, "{text:?}");
}

#[test]
fn syntax_error_marks_editor() {
    let dir = tempfile::tempdir().unwrap();
    let error = DriverError {
        position: Some(7),
        ..DriverError::new(ErrorKind::Syntax, "syntax error at or near \"FROM\"")
    };
    let (mut h, _) = harness_with_script(
        &dir,
        FakeScript {
            query_error: Some(error),
            ..Default::default()
        },
    );
    expand_to_orders(&mut h);
    double_click(&mut h, "orders");
    wait_for_label(&mut h, "at line 1, column 8");
}

#[test]
fn script_runs_on_the_tab_database() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_script(
        &dir,
        FakeScript {
            tree: vec![node(&NodePath::root(), "other", NodeKind::Database, true)],
            databases: vec!["other".into()],
            ..Default::default()
        },
    );
    h.get_by_label("Expand fake-conn").click();
    wait_for_label(&mut h, "other");
    context_menu(&mut h, "other", "New query");
    replace_text(&mut h, "select 1; select 2;");
    press(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter);
    step_until(&mut h, "the script", |_| driver.executed_on().len() == 2);
    let other = Some("other".to_string());
    assert_eq!(
        driver.executed_on(),
        [
            (other.clone(), "select 1".to_string()),
            (other, "select 2".to_string())
        ]
    );
}

#[test]
fn startup_notice_shows_as_a_toast() {
    let dir = tempfile::tempdir().unwrap();
    let (mut deps, _) = deps_with(dir.path(), FakeScript::default());
    deps.startup_notice = Some("History and snippets will not be saved: locked".into());
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    h.get_by_label("History and snippets will not be saved: locked");
}

#[test]
fn appearance_switch_applies() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    assert!(
        h.ctx.global_style().visuals.dark_mode,
        "System falls back to dark"
    );
    h.get_by_label("File").click();
    h.run_steps(2);
    h.get_by_label_contains("Settings…").click();
    h.run_steps(2);
    h.get_by_label("Theme");
    h.get_by_label("Light").click_accesskit();
    h.run_steps(2);
    h.get_by_label("Apply").click_accesskit();
    step_until(&mut h, "the light theme", |h| {
        !h.ctx.global_style().visuals.dark_mode
    });
    let saved = Settings::load(dir.path()).unwrap();
    assert_eq!(saved.theme, ThemePref::Light);
    // Apply keeps the dialog open; Save closes it.
    h.get_by_label("Save").click_accesskit();
    step_until(&mut h, "the settings dialog closes", |h| {
        h.query_by_label("Apply").is_none()
    });
    assert!(!h.ctx.global_style().visuals.dark_mode);
}

#[test]
fn storage_warning_outlives_the_startup_notice() {
    let dir = tempfile::tempdir().unwrap();
    let (mut deps, driver) = deps_with(dir.path(), FakeScript::default());
    let reason = "History and snippets will not be saved: locked";
    deps.startup_notice = Some(reason.into());
    deps.storage_warning = Some(reason.into());
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    h.get_by_label(reason);
    h.get_by_label("Not saved");
    // The notice is a toast that goes away; the warning stays.
    open_orders(&mut h);
    assert_eq!(driver.executed().len(), 1);
    h.run_steps(900);
    assert!(h.query_by_label(reason).is_none(), "the toast expired");
    h.get_by_label("Not saved").hover();
    h.run_steps(100);
    h.get_by_label(reason);
}

#[test]
fn script_result_error_uses_its_statement_origin() {
    let dir = tempfile::tempdir().unwrap();
    let error = DriverError {
        position: Some(7),
        ..DriverError::new(ErrorKind::Syntax, "fetch failed")
    };
    let (mut h, _) = harness_with_script(
        &dir,
        FakeScript {
            fail_after_chunks: Some((1, error)),
            ..Default::default()
        },
    );
    open_orders(&mut h);
    replace_text(&mut h, "select 1;\nselect 2;");
    press(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter);
    wait_for_label(&mut h, "Result 2");
    h.get_by_label("Result 2").click();
    h.run_steps(2);
    // The second statement's next page fails; its position is inside statement 2.
    h.get_by_label("Fetch all").click();
    wait_for_label(&mut h, "fetch failed");
    h.get_by_label("at line 2, column 8");
}

fn is_disabled(h: &Harness<'static, SqlrApp>, label: &str) -> bool {
    h.get_by_label(label).accesskit_node().is_disabled()
}

#[test]
fn production_confirms_destructive() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_profile(&dir, "production");
    open_orders(&mut h);
    replace_text(&mut h, "delete from t");
    press(&mut h, Modifiers::COMMAND, Key::Enter);
    wait_for_label(
        &mut h,
        "This will DELETE without WHERE on fake-conn (production profile).",
    );
    h.get_by_label("Cancel").click();
    h.run_steps(5);
    assert!(h.query_by_label_contains("This will").is_none());
    assert!(
        !driver.executed().iter().any(|s| s.contains("delete")),
        "{:?}",
        driver.executed()
    );
}

#[test]
fn production_confirms_writes_and_runs_on_confirm() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_profile(&dir, "production");
    open_orders(&mut h);
    replace_text(&mut h, "update t set a = 1 where id = 2");
    press(&mut h, Modifiers::COMMAND, Key::Enter);
    wait_for_label(&mut h, "Run 1 statement that modifies data on fake-conn?");
    // The dialog's "Run" comes after the toolbar's.
    h.get_all_by_label("Run").last().unwrap().click();
    step_until(&mut h, "the update to run", |_| {
        driver.executed().iter().any(|s| s.starts_with("update"))
    });
}

#[test]
fn commit_rollback_buttons_follow_state() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_profile(&dir, "production");
    assert!(is_disabled(&h, "Commit"));
    assert!(is_disabled(&h, "Rollback"));
    open_orders(&mut h);
    // Manual commit: the explorer's query opened a transaction.
    step_until(&mut h, "Commit to enable", |h| !is_disabled(h, "Commit"));
    assert!(!is_disabled(&h, "Rollback"));
    h.get_by_label_contains("Transaction: 1 statement");
    assert_eq!(driver.executed()[0], "BEGIN");
    h.get_by_label("Commit").click();
    step_until(&mut h, "the commit", |h| is_disabled(h, "Commit"));
    assert_eq!(driver.executed().last().map(String::as_str), Some("COMMIT"));
    assert!(h.query_by_label_contains("Transaction:").is_none());
}

#[test]
fn close_tab_with_open_transaction_asks() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_profile(&dir, "production");
    open_orders(&mut h);
    step_until(&mut h, "the transaction", |h| !is_disabled(h, "Commit"));
    press(&mut h, Modifiers::COMMAND, Key::W);
    wait_for_label(&mut h, "Roll back");
    assert_eq!(h.state().tab_titles(), ["Query 1", "orders"], "still open");
    h.get_by_label("Roll back").click();
    step_until(&mut h, "the tab to close", |h| {
        h.state().tab_titles() == ["Query 1"]
    });
    assert!(driver.executed().iter().any(|s| s == "ROLLBACK"));
}

#[test]
fn profile_color_tints_tab() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = harness_with_profile(&dir, "test");
    open_orders(&mut h);
    assert_eq!(h.state().tab_tints(), [None, Some([0xE0, 0xA0, 0x30])]);
}

/// Types `password` into the secrets prompt and connects.
fn enter_password(h: &mut Harness<'static, SqlrApp>, password: &str) {
    wait_for_label(h, "Secrets for fake-conn");
    h.run_steps(3);
    // The field is labelled by its caption.
    h.get_by_label("Password").click();
    h.run_steps(2);
    type_text(h, password);
    h.get_by_label("Connect").click();
    h.run_steps(2);
}

#[test]
fn unsaved_password_is_asked_on_connect() {
    let dir = tempfile::tempdir().unwrap();
    let script = FakeScript {
        password: Some("hunter2".into()),
        ..Default::default()
    };
    let (deps, _) = deps_with(dir.path(), script);
    let mut cfg = deps.repo.list().unwrap().remove(0);
    cfg.save_secrets = false;
    deps.repo.save(&cfg, &Secrets::default()).unwrap();
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    h.get_by_label("Expand fake-conn").click();
    // A mistyped password fails the connect and is asked for again.
    enter_password(&mut h, "wrong");
    wait_for_label(&mut h, "failed");
    context_menu(&mut h, "fake-conn", "Connect");
    enter_password(&mut h, "hunter2");
    wait_for_label(&mut h, "connected");
    // Asked once per app run.
    context_menu(&mut h, "fake-conn", "Disconnect");
    context_menu(&mut h, "fake-conn", "Connect");
    wait_for_label(&mut h, "connected");
    assert!(h.query_by_label("Secrets for fake-conn").is_none());
}

/// A fake whose `SELECT`s return `s.orders` (`id` primary key, `total`) with origins.
fn orders_table_script() -> FakeScript {
    use sqlr_core::{
        ColumnKind, ColumnMeta, ColumnOrigin, DisplayHint, TableColumn, TableInfo, TableRef, Value,
    };
    let table = TableRef {
        database: None,
        schema: Some("s".into()),
        name: "orders".into(),
    };
    let column = |name: &str| {
        ColumnMeta::new(name, "int8", ColumnKind::Int, DisplayHint::Number).with_origin(Some(
            ColumnOrigin {
                table: table.clone(),
                column: name.into(),
            },
        ))
    };
    let table_column = |name: &str, nullable| TableColumn {
        name: name.into(),
        type_name: "int8".into(),
        nullable,
        default: None,
        generated: false,
    };
    FakeScript {
        editable: true,
        quote_idents: true,
        result: Some(sqlr_testkit::FakeResult {
            columns: vec![column("id"), column("total")],
            rows: vec![
                vec![Value::Int(1), Value::Int(10)],
                vec![Value::Int(2), Value::Int(20)],
            ],
        }),
        tables: vec![TableInfo {
            table,
            columns: vec![table_column("id", false), table_column("total", true)],
            primary_key: vec!["id".into()],
            unique_keys: vec![],
        }],
        ..Default::default()
    }
}

#[test]
fn edit_preview_apply_flow() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_script(&dir, orders_table_script());
    expand_to_orders(&mut h);
    double_click(&mut h, "orders");
    wait_for_label(&mut h, "20");
    wait_for_label(&mut h, "No pending changes");
    double_click(&mut h, "10");
    h.event(egui::Event::Text("15".into()));
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::Enter);
    wait_for_label(&mut h, "1 pending change");
    h.get_by_label("Preview").click();
    h.run_steps(5);
    let preview = h
        .state()
        .changes_preview()
        .expect("the preview dialog is open");
    assert!(
        preview.contains("UPDATE \"s\".\"orders\" SET \"total\" = 15 WHERE \"id\" = 1;"),
        "{preview}"
    );
    let selects_before = driver
        .executed()
        .iter()
        .filter(|s| s.starts_with("SELECT"))
        .count();
    h.get_by_label("Apply changes").click();
    step_until(&mut h, "the changes to apply and the data to reload", |h| {
        h.query_by_label("No pending changes").is_some()
            && driver
                .executed()
                .iter()
                .filter(|s| s.starts_with("SELECT"))
                .count()
                > selects_before
    });
    let executed = driver.executed();
    let begin = executed.iter().position(|s| s == "BEGIN").unwrap();
    assert_eq!(
        executed[begin..begin + 3],
        [
            "BEGIN",
            "UPDATE \"s\".\"orders\" SET \"total\" = ? WHERE \"id\" = ?",
            "COMMIT"
        ]
    );
    let with = driver.executed_with();
    assert_eq!(
        with.last().unwrap().1,
        vec![sqlr_core::Value::Int(15), sqlr_core::Value::Int(1)]
    );
    assert!(h.state().changes_preview().is_none());
}

#[test]
fn discarding_pending_edits_clears_them() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _driver) = harness_with_script(&dir, orders_table_script());
    expand_to_orders(&mut h);
    double_click(&mut h, "orders");
    wait_for_label(&mut h, "20");
    double_click(&mut h, "10");
    h.event(egui::Event::Text("15".into()));
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::Enter);
    wait_for_label(&mut h, "1 pending change");
    h.get_by_label("Discard").click();
    wait_for_label(&mut h, "No pending changes");
}

#[test]
fn a_driver_without_editing_shows_the_result_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _driver) = harness_with_script(
        &dir,
        FakeScript {
            editable: false,
            ..Default::default()
        },
    );
    open_orders(&mut h);
    wait_for_label(&mut h, "Read-only: driver does not support editing");
}

#[test]
fn status_bar_add_row_adds_a_pending_row() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _driver) = harness_with_script(&dir, orders_table_script());
    expand_to_orders(&mut h);
    double_click(&mut h, "orders");
    wait_for_label(&mut h, "No pending changes");
    h.get_by_label("Add row").click_accesskit();
    wait_for_label(&mut h, "1 pending change");
}

#[test]
fn server_filter_waits_for_pending_changes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_script(&dir, orders_table_script());
    expand_to_orders(&mut h);
    double_click(&mut h, "orders");
    wait_for_label(&mut h, "20");
    double_click(&mut h, "10");
    h.event(egui::Event::Text("15".into()));
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::Enter);
    wait_for_label(&mut h, "1 pending change");
    let runs = driver.executed().len();
    // The WHERE field has only a hint: it is the last empty text input on screen (the
    // explorer's filter comes earlier).
    let field = h
        .get_all(
            egui_kittest::kittest::By::new()
                .role(egui::accesskit::Role::TextInput)
                .value(""),
        )
        .last()
        .expect("the WHERE field");
    field.click();
    h.run_steps(2);
    h.event(egui::Event::Text("id > 1".into()));
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(5);
    wait_for_label(&mut h, "Apply or discard the pending changes first");
    assert_eq!(driver.executed().len(), runs, "nothing re-ran");
    h.get_by_label("1 pending change");
}

#[test]
fn server_filter_reruns_the_statement() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_script(&dir, orders_table_script());
    expand_to_orders(&mut h);
    double_click(&mut h, "orders");
    wait_for_label(&mut h, "20");
    let field = h
        .get_all(
            egui_kittest::kittest::By::new()
                .role(egui::accesskit::Role::TextInput)
                .value(""),
        )
        .last()
        .expect("the WHERE field");
    field.click();
    h.run_steps(2);
    h.event(egui::Event::Text("total > 10".into()));
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::Enter);
    step_until(&mut h, "the filtered query", |_| {
        driver
            .executed()
            .iter()
            .any(|sql| sql.contains("total > 10"))
    });
}

#[test]
fn result_without_origins_explains_why_it_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _driver) = harness_with_script(
        &dir,
        FakeScript {
            editable: true,
            ..Default::default()
        },
    );
    open_orders(&mut h);
    wait_for_label(&mut h, "Read-only: computed columns");
}

#[test]
fn messages_tab_lists_statements() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _driver) = harness_with_driver(&dir);
    open_orders(&mut h);
    h.get_by_label("Messages").click();
    h.run_steps(2);
    h.get_by_label_contains("SELECT * FROM orders · 500 rows (more available)");
    replace_text(&mut h, "select 1; select 2;");
    press(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter);
    step_until(&mut h, "the script", |h| {
        h.query_by_label("Result 2").is_some()
    });
    h.get_by_label("Messages").click();
    h.run_steps(2);
    h.get_by_label_contains("select 1 · 500 rows");
    h.get_by_label_contains("select 2 · 500 rows");
    h.get_by_label("Copy");
}

#[test]
fn export_dialog_writes_file() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _driver) = harness_with_driver(&dir);
    let path = dir.path().join("orders.csv");
    h.state_mut().set_file_dialog_answer(Some(path.clone()));
    open_orders(&mut h);
    h.get_by_label("Export…").click();
    h.run_steps(2);
    h.get_by_label("Whole result (re-run)");
    h.get_by_label("Export").click();
    step_until(&mut h, "the export", |_| path.exists());
    wait_for_label(&mut h, &format!("Exported 1200 rows to {}", path.display()));
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 1201);
    assert_eq!(text.lines().next(), Some("n"));
}

#[test]
fn export_of_a_writing_statement_offers_only_loaded_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_driver(&dir);
    let path = dir.path().join("out.csv");
    h.state_mut().set_file_dialog_answer(Some(path.clone()));
    expand_to_orders(&mut h);
    double_click(&mut h, "orders");
    wait_for_label(&mut h, "Export…");
    replace_text(&mut h, "INSERT INTO t VALUES (1) RETURNING *");
    press(&mut h, Modifiers::COMMAND, Key::Enter);
    step_until(&mut h, "the INSERT", |_| {
        driver
            .executed()
            .last()
            .is_some_and(|s| s.starts_with("INSERT"))
    });
    wait_for_label(&mut h, "500 rows (more available)");
    h.get_by_label("Export…").click();
    h.run_steps(2);
    h.get_by_label("Loaded rows (500)");
    assert!(h.query_by_label("Whole result (re-run)").is_none());
    let runs = driver.executed().len();
    h.get_by_label("Export").click();
    step_until(&mut h, "the export", |_| path.exists());
    assert_eq!(
        driver.executed().len(),
        runs,
        "the INSERT did not run again"
    );
}

#[test]
fn export_of_a_complete_result_defaults_to_loaded_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_script(&dir, orders_table_script());
    let path = dir.path().join("orders.csv");
    h.state_mut().set_file_dialog_answer(Some(path.clone()));
    expand_to_orders(&mut h);
    double_click(&mut h, "orders");
    wait_for_label(&mut h, "2 rows");
    h.get_by_label("Export…").click();
    h.run_steps(2);
    // Offered, with the note that it runs on another session; not the default.
    assert!(h.query_by_label_contains("separate session").is_none());
    h.get_by_label("Whole result (re-run)").click();
    h.run_steps(2);
    h.get_by_label_contains("separate session");
    h.get_by_label("Loaded rows (2)").click();
    h.run_steps(2);
    assert!(h.query_by_label_contains("separate session").is_none());
    let runs = driver.executed().len();
    h.get_by_label("Export").click();
    step_until(&mut h, "the export", |_| path.exists());
    assert_eq!(
        driver.executed().len(),
        runs,
        "the loaded rows were written"
    );
}

#[test]
fn explorer_import_runs_with_a_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let script = orders_table_script();
    let (mut h, driver) = harness_with_script(&dir, script);
    let path = dir.path().join("in.csv");
    std::fs::write(&path, "ID,Total,extra\n7,70,x\n8,80,y\n").unwrap();
    h.state_mut().set_file_dialog_answer(Some(path));
    expand_to_orders(&mut h);
    context_menu(&mut h, "orders", "Import data…");
    h.get_by_label("Choose file…").click();
    // The preview shows the file's rows; columns are mapped by name.
    wait_for_label(&mut h, "80");
    h.get_by_label("Run import").click();
    wait_for_label(&mut h, "Read 2 rows, wrote 2, rejected 0");
    let inserts: Vec<_> = driver
        .executed_with()
        .into_iter()
        .filter(|(sql, _)| sql.starts_with("INSERT"))
        .collect();
    assert_eq!(inserts.len(), 2);
    assert_eq!(
        inserts[0].0,
        "INSERT INTO \"s\".\"orders\" (\"id\", \"total\") VALUES (?, ?)"
    );
    assert_eq!(
        inserts[1].1,
        vec![sqlr_core::Value::Int(8), sqlr_core::Value::Int(80)]
    );
}

#[test]
fn import_asks_first_when_the_profile_confirms_writes() {
    let dir = tempfile::tempdir().unwrap();
    let (deps, driver) = deps_with(dir.path(), orders_table_script());
    let mut cfg = deps.repo.list().unwrap().remove(0);
    cfg.profile = Some("production".into());
    deps.repo.save(&cfg, &Secrets::default()).unwrap();
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    let path = dir.path().join("in.csv");
    std::fs::write(&path, "id,total\n7,70\n8,80\n").unwrap();
    h.state_mut().set_file_dialog_answer(Some(path));
    expand_to_orders(&mut h);
    context_menu(&mut h, "orders", "Import data…");
    h.get_by_label("Choose file…").click();
    wait_for_label(&mut h, "80");
    h.get_by_label("Run import").click();
    wait_for_label(
        &mut h,
        "Insert the file's rows into orders? The profile confirms writes.",
    );
    h.run_steps(5);
    assert!(
        !driver.executed().iter().any(|s| s.starts_with("INSERT")),
        "nothing is written before the answer"
    );
    h.get_by_label("Import").click();
    wait_for_label(&mut h, "Read 2 rows, wrote 2, rejected 0");
}

#[test]
fn import_saves_its_rejected_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _driver) = harness_with_script(&dir, orders_table_script());
    let input = dir.path().join("in.csv");
    std::fs::write(&input, "id,total\n7,70\nabc,80\n").unwrap();
    h.state_mut().set_file_dialog_answer(Some(input));
    expand_to_orders(&mut h);
    context_menu(&mut h, "orders", "Import data…");
    h.get_by_label("Choose file…").click();
    wait_for_label(&mut h, "abc");
    h.get_by_label("Skip bad rows and report them").click();
    h.run_steps(2);
    h.get_by_label("Run import").click();
    wait_for_label(&mut h, "Read 2 rows, wrote 1, rejected 1");
    h.run_steps(2);
    let saved = dir.path().join("rejected.csv");
    h.state_mut().set_file_dialog_answer(Some(saved.clone()));
    h.get_by_label("Save rejected rows…").click_accesskit();
    wait_for_label(
        &mut h,
        &format!("Saved the rejected rows to {}", saved.display()),
    );
    let text = std::fs::read_to_string(&saved).unwrap();
    assert!(text.starts_with("line,reason\n3,"), "{text}");
}

/// Renders the configuration windows to `docs/media/screenshots/` (needs a GPU adapter):
/// `cargo test -p sqlr-app --test smoke config_screenshots -- --ignored`.
#[test]
#[ignore = "writes screenshots; needs a wgpu adapter"]
fn config_screenshots() {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/media/screenshots");
    std::fs::create_dir_all(&out).unwrap();
    render_config_screens(&out, ThemePref::Dark, "dark");
    render_config_screens(&out, ThemePref::Light, "light");
}

fn render_config_screens(out: &std::path::Path, theme: ThemePref, suffix: &str) {
    let dir = tempfile::tempdir().unwrap();
    let (mut deps, _) = deps_with(dir.path(), FakeScript::default());
    deps.settings.theme = theme;
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    sqlr_ui::theme::install_fonts(&h.ctx, &[]);
    h.run_steps(2);

    h.get_by_label("File").click();
    h.run_steps(2);
    h.get_by_label_contains("Settings…").click();
    h.run_steps(5);
    h.event(egui::Event::PointerMoved(egui::pos2(1390.0, 890.0)));
    h.run_steps(2);
    h.render()
        .expect("render")
        .save(out.join(format!("config-settings-{suffix}.png")))
        .unwrap();
    h.get_by_label("Cancel").click();
    h.run_steps(3);

    let plus = h.get_by_label("New connection").accesskit_node().id();
    h.get_by_label("File").click();
    h.run_steps(2);
    h.get_all_by_label("New connection")
        .find(|n| n.accesskit_node().id() != plus)
        .expect("File menu has a New connection item")
        .click();
    h.run_steps(5);
    h.event(egui::Event::PointerMoved(egui::pos2(1390.0, 890.0)));
    h.run_steps(2);
    h.render()
        .expect("render")
        .save(out.join(format!("config-connection-{suffix}.png")))
        .unwrap();
    h.get_by_label("Cancel").click();
    h.run_steps(3);

    h.get_by_label("File").click();
    h.run_steps(2);
    h.get_by_label("Connection profiles…").click();
    h.run_steps(5);
    h.event(egui::Event::PointerMoved(egui::pos2(1390.0, 890.0)));
    h.run_steps(2);
    h.render()
        .expect("render")
        .save(out.join(format!("config-profiles-{suffix}.png")))
        .unwrap();
}

/// Renders the main window in both themes to `docs/media/screenshots/` (needs a GPU adapter):
/// `cargo test -p sqlr-app --test smoke screenshots -- --ignored`.
#[test]
#[ignore = "writes screenshots; needs a wgpu adapter"]
fn screenshots() {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/media/screenshots");
    std::fs::create_dir_all(&out).unwrap();
    for (name, pref) in [("dark", ThemePref::Dark), ("light", ThemePref::Light)] {
        let dir = tempfile::tempdir().unwrap();
        let (mut deps, _) = deps_with(dir.path(), FakeScript::default());
        deps.settings.theme = pref;
        let app = SqlrApp::new(deps, &egui::Context::default());
        let mut h = Harness::builder()
            .with_size((1400.0, 860.0))
            .with_step_dt(0.01)
            .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
        sqlr_ui::theme::install_fonts(&h.ctx, &[]);
        h.run_steps(2);
        open_orders(&mut h);
        replace_text(
            &mut h,
            "-- Orders of the last week\nSELECT o.id, o.total, 'paid' AS state\nFROM orders o\nWHERE o.total > 100\n  AND o.created_at > now() - interval '7 days'\nORDER BY o.total DESC;",
        );
        // The tree row and the result both carry the name: take the first (the tree).
        h.get_all_by_label("orders")
            .next()
            .expect("the orders row")
            .click();
        h.run_steps(60);
        let image = h.render().expect("render");
        image.save(out.join(format!("main-{name}.png"))).unwrap();
    }
}

/// Renders the editor (line numbers, statement tint, a selected statement) in both themes to
/// `docs/media/screenshots/editing-{dark,light}.png` (needs a GPU adapter):
/// `cargo test -p sqlr-app --test smoke editing_screenshots -- --ignored`.
#[test]
#[ignore = "writes screenshots; needs a wgpu adapter"]
fn editing_screenshots() {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/media/screenshots");
    std::fs::create_dir_all(&out).unwrap();
    for (name, pref) in [("dark", ThemePref::Dark), ("light", ThemePref::Light)] {
        let dir = tempfile::tempdir().unwrap();
        let (mut deps, _) = deps_with(dir.path(), FakeScript::default());
        deps.settings.theme = pref;
        let app = SqlrApp::new(deps, &egui::Context::default());
        let mut h = Harness::builder()
            .with_size((1400.0, 860.0))
            .with_step_dt(0.01)
            .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
        sqlr_ui::theme::install_fonts(&h.ctx, &[]);
        h.run_steps(2);
        open_orders(&mut h);
        replace_text(
            &mut h,
            "-- Weekly revenue\nSELECT date_trunc('week', o.created_at) AS week,\n       count(*) AS orders,\n       sum(o.total) AS revenue\nFROM orders o\nWHERE o.status = 'paid'\nGROUP BY 1\nORDER BY 1;\n\n-- Follow-up: the biggest orders\nSELECT o.id, o.total\nFROM orders o\nORDER BY o.total DESC\nLIMIT 20;",
        );
        h.run_steps(10);
        let image = h.render().expect("render");
        image.save(out.join(format!("editing-{name}.png"))).unwrap();
    }
}

/// Renders the shell (toolbar, chip, breadcrumb, status bar and a toast) in both themes to
/// `docs/media/screenshots/shell-{dark,light}.png` (needs a GPU adapter):
/// `cargo test -p sqlr-app --test smoke shell_screenshots -- --ignored`.
#[test]
#[ignore = "writes screenshots; needs a wgpu adapter"]
fn shell_screenshots() {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/media/screenshots");
    std::fs::create_dir_all(&out).unwrap();
    for (name, pref) in [("dark", ThemePref::Dark), ("light", ThemePref::Light)] {
        let dir = tempfile::tempdir().unwrap();
        let (mut deps, _) = deps_with(dir.path(), FakeScript::default());
        deps.settings.theme = pref;
        deps.startup_notice = Some("Could not read connection folders: permission denied".into());
        let app = SqlrApp::new(deps, &egui::Context::default());
        let mut h = Harness::builder()
            .with_size((1400.0, 860.0))
            .with_step_dt(0.01)
            .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
        sqlr_ui::theme::install_fonts(&h.ctx, &[]);
        h.run_steps(2);
        open_orders(&mut h);
        replace_text(
            &mut h,
            "-- Orders of the last week\nSELECT o.id, o.total\nFROM orders o\nWHERE o.total > 100;",
        );
        h.run_steps(60);
        let image = h.render().expect("render");
        image.save(out.join(format!("shell-{name}.png"))).unwrap();
    }
}

#[test]
fn toolbar_button_shows_a_single_tooltip() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    h.run_steps(100);
    h.get_by_label("Run").hover();
    h.run_steps(100);
    // The button plus its one tooltip; a second tooltip would duplicate "Run".
    assert_eq!(
        h.get_all_by_label("Run").count(),
        1,
        "the toolbar button stacks two tooltips"
    );
}

#[test]
fn completion_uses_the_tabs_database() {
    let dir = tempfile::tempdir().unwrap();
    let column = |name: &str| CatalogColumn {
        name: name.into(),
        type_name: "int4".into(),
    };
    let (mut h, _driver) = harness_with_script(
        &dir,
        FakeScript {
            tree: vec![node(&NodePath::root(), "other", NodeKind::Database, true)],
            databases: vec!["other".into()],
            own_database: Some("s".into()),
            default_schemas: vec!["s".into()],
            database_is_schema: true,
            catalog: vec![
                SchemaCatalog {
                    schema: "s".into(),
                    relations: vec![CatalogRelation {
                        name: "orders".into(),
                        kind: RelationKind::Table,
                        columns: vec![column("id"), column("total")],
                    }],
                },
                SchemaCatalog {
                    schema: "other".into(),
                    relations: vec![CatalogRelation {
                        name: "orders".into(),
                        kind: RelationKind::Table,
                        columns: vec![column("id"), column("other_total")],
                    }],
                },
            ],
            ..Default::default()
        },
    );
    h.get_by_label("Expand fake-conn").click();
    wait_for_label(&mut h, "other");
    context_menu(&mut h, "other", "New query");

    // Ctrl+Space lists the tables of the database the tab runs on, not the connection's own.
    focus_editor_end(&mut h);
    type_text(&mut h, "select * from ");
    press(&mut h, Modifiers::CTRL, Key::Space);
    step_until(&mut h, "the tab database's tables", |h| {
        h.query_by_label_contains("orders").is_some()
    });
    // Ctrl+Space toggles: the second press closes the popup, the third reopens it.
    press(&mut h, Modifiers::CTRL, Key::Space);
    assert!(
        h.query_by_label_contains("orders").is_none(),
        "the second Ctrl+Space closes the popup"
    );
    press(&mut h, Modifiers::CTRL, Key::Space);
    step_until(&mut h, "the reopened table list", |h| {
        h.query_by_label_contains("orders").is_some()
    });
    press(&mut h, Modifiers::NONE, Key::Escape);

    // Columns after an alias resolve in that database too.
    replace_text(&mut h, "select * from orders as p");
    focus_editor_end(&mut h);
    press(&mut h, Modifiers::NONE, Key::Home);
    for _ in 0..7 {
        press(&mut h, Modifiers::NONE, Key::ArrowRight);
    }
    press(&mut h, Modifiers::NONE, Key::Delete);
    type_text(&mut h, "p.");
    press(&mut h, Modifiers::NONE, Key::Escape);
    press(&mut h, Modifiers::CTRL, Key::Space);
    step_until(&mut h, "the tab database's columns", |h| {
        h.query_by_label_contains("other_total").is_some()
    });
}

#[test]
fn connections_can_be_organized_in_folders_by_dragging() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = harness_with_driver(&dir);

    h.get_by_label("New folder").click();
    h.run_steps(3);
    let field = h
        .get_all(
            egui_kittest::kittest::By::new()
                .role(egui::accesskit::Role::TextInput)
                .value(""),
        )
        .last()
        .expect("the new folder field");
    field.focus();
    h.run_steps(1);
    h.event(egui::Event::Text("Prod".into()));
    h.run_steps(1);
    h.key_press(Key::Enter);
    wait_for_label(&mut h, "Prod");

    drag_onto(&mut h, "fake-conn", "Prod");
    let repo = ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
    step_until(&mut h, "the connection moved into its folder", |_| {
        repo.list()
            .is_ok_and(|list| list[0].group.as_deref() == Some("Prod"))
    });
    assert_eq!(repo.folders().unwrap(), vec!["Prod"]);
    h.get_by_label("fake-conn");
}

#[test]
fn folders_can_be_nested_from_the_explorer() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = harness_with_driver(&dir);

    // A top-level folder, then one inside it.
    h.get_by_label("New folder").click();
    h.run_steps(3);
    let field = h
        .get_all(
            egui_kittest::kittest::By::new()
                .role(egui::accesskit::Role::TextInput)
                .value(""),
        )
        .last()
        .expect("the new folder field");
    field.focus();
    h.run_steps(1);
    h.event(egui::Event::Text("Prod".into()));
    h.run_steps(1);
    h.key_press(Key::Enter);
    wait_for_label(&mut h, "Prod");

    h.get_by_label("Prod").click_secondary();
    h.run_steps(2);
    h.get_all_by_label("New folder")
        .last()
        .expect("the folder menu entry")
        .click();
    h.run_steps(3);
    let field = h
        .get_all(
            egui_kittest::kittest::By::new()
                .role(egui::accesskit::Role::TextInput)
                .value(""),
        )
        .last()
        .expect("the nested folder field");
    field.focus();
    h.run_steps(1);
    h.event(egui::Event::Text("Team".into()));
    h.run_steps(1);
    h.key_press(Key::Enter);

    let repo = ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
    step_until(&mut h, "the nested folder", |_| {
        repo.folders().is_ok_and(|f| f == vec!["Prod", "Prod/Team"])
    });
    h.run_steps(2);
    h.get_by_label("Team");

    // A connection dropped on the nested folder belongs to its full path.
    drag_onto(&mut h, "fake-conn", "Team");
    step_until(&mut h, "the connection in the nested folder", |_| {
        repo.list()
            .is_ok_and(|list| list[0].group.as_deref() == Some("Prod/Team"))
    });
}

/// A harness whose statements containing `pg_sleep` stream until cancelled.
fn blocking_harness(dir: &tempfile::TempDir) -> (Harness<'static, SqlrApp>, Arc<FakeDriver>) {
    harness_with_script(
        dir,
        FakeScript {
            block_fetch_matching: Some("pg_sleep".into()),
            ..FakeScript::default()
        },
    )
}

/// Opens `orders` and starts a statement that runs until it is stopped.
fn start_blocking_run(h: &mut Harness<'static, SqlrApp>) {
    open_orders(h);
    replace_text(h, "select pg_sleep(60)");
    press(h, Modifiers::COMMAND, Key::Enter);
    step_until(h, "the run starts", |h| !is_disabled(h, "Stop"));
}

#[test]
fn toolbar_run_stop_toggles() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = blocking_harness(&dir);
    open_orders(&mut h);
    replace_text(&mut h, "select pg_sleep(60)");
    h.get_by_label("Run").click();
    step_until(&mut h, "the run starts", |h| !is_disabled(h, "Stop"));
    h.get_by_label("Stop").click();
    step_until(&mut h, "the run is cancelled", |h| is_disabled(h, "Stop"));
    h.get_by_label_contains("cancelled");
}

/// Whether the toolbar's Stop (the topmost "Stop": the palette lists one too) is disabled.
fn stop_disabled(h: &Harness<'static, SqlrApp>) -> bool {
    h.get_all_by_label("Stop")
        .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .expect("the toolbar's Stop")
        .accesskit_node()
        .is_disabled()
}

/// Esc reaches the shell this frame, and the run is still going afterwards.
fn esc_leaves_run(h: &mut Harness<'static, SqlrApp>, why: &str) {
    press(h, Modifiers::NONE, Key::Escape);
    h.run_steps(10);
    assert!(!stop_disabled(h), "Esc stopped the run: {why}");
}

/// Esc with the editor focused and nothing else to dismiss stops the run.
fn esc_stops(h: &mut Harness<'static, SqlrApp>) {
    focus_editor_end(h);
    press(h, Modifiers::NONE, Key::Escape);
    step_until(h, "the run is cancelled", stop_disabled);
}

#[test]
fn esc_stops_run() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = blocking_harness(&dir);
    start_blocking_run(&mut h);
    // The first Esc clears the selection, the second stops the run.
    press(&mut h, Modifiers::COMMAND, Key::A);
    esc_leaves_run(&mut h, "it had a selection to clear");
    press(&mut h, Modifiers::NONE, Key::Escape);
    step_until(&mut h, "the run is cancelled", stop_disabled);
}

#[test]
fn esc_closes_the_palette_before_stopping() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = blocking_harness(&dir);
    start_blocking_run(&mut h);
    press(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::P);
    h.run_steps(5);
    assert!(h.state().palette_open(), "⌘K opens the palette");
    esc_leaves_run(&mut h, "the palette was open");
    assert!(!h.state().palette_open(), "Esc closed the palette");
    esc_stops(&mut h);
}

#[test]
fn esc_closes_a_modal_before_stopping() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = blocking_harness(&dir);
    start_blocking_run(&mut h);
    context_menu(&mut h, "fake-conn", "Delete");
    wait_for_label(&mut h, "Cancel");
    esc_leaves_run(&mut h, "the delete confirmation was open");
    assert!(h.query_by_label("Cancel").is_none(), "Esc closed the modal");
    esc_stops(&mut h);
}

#[test]
fn esc_closes_the_chip_popup_before_stopping() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = blocking_harness(&dir);
    start_blocking_run(&mut h);
    focus_editor_end(&mut h);
    let chip = h
        .get_all_by_label("fake-conn")
        .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .unwrap()
        .accesskit_node()
        .id();
    h.get_all_by_label("fake-conn")
        .find(|n| n.accesskit_node().id() == chip)
        .unwrap()
        .click();
    h.run_steps(2);
    h.get_by_label("CONNECTIONS");
    esc_leaves_run(&mut h, "the chip popup was open");
    assert!(
        h.query_by_label("CONNECTIONS").is_none(),
        "Esc closed the popup"
    );
    esc_stops(&mut h);
}

#[test]
fn status_shows_connection_and_caret() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    open_orders(&mut h);
    replace_text(&mut h, "select 1;\nsele");
    h.get_by_label("Connected");
    h.get_by_label("Ln 2, Col 5");
    h.get_by_label("UTF-8");
    h.get_by_label("Auto-commit");
}

#[test]
fn cmd_n_and_cmd_w() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    open_orders(&mut h);
    let before = h.state().tab_titles().len();
    h.key_press_modifiers(Modifiers::COMMAND, Key::N);
    h.run_steps(3);
    let titles = h.state().tab_titles();
    assert_eq!(titles.len(), before + 1, "{titles:?}");
    // The new tab (focused, last in the dock) is on the active tab's connection.
    let conns = h.state().tab_conns();
    assert_eq!(conns.last(), Some(&Some("c1".to_string())), "{conns:?}");
    h.key_press_modifiers(Modifiers::COMMAND, Key::W);
    h.run_steps(3);
    assert_eq!(h.state().tab_titles().len(), before);
}

#[test]
fn cmd_comma_opens_settings() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    h.run_steps(3);
    h.get_by_label("Theme");
}

#[test]
fn notices_become_toasts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("profiles.toml"), "this is = = not toml").unwrap();
    let mut h = harness(&dir);
    step_until(&mut h, "the profiles notice", |h| {
        h.state()
            .toast_texts()
            .iter()
            .any(|t| t.contains("connection profiles"))
    });
    h.run_steps(2);
    assert!(
        h.query_by_label_contains("connection profiles").is_some(),
        "the toast is on screen"
    );
}

#[test]
fn breadcrumb_reveals_the_connection() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    open_orders(&mut h);
    // Right of the explorer, below the toolbar.
    let breadcrumb = h
        .get_all_by_label("fake-conn")
        .find(|n| n.rect().left() > 270.0 && n.rect().top() > 50.0)
        .expect("the breadcrumb's connection");
    assert_ne!(
        h.state().explorer_selection(),
        Some(("c1".to_string(), NodePath::root()))
    );
    breadcrumb.click();
    h.run_steps(2);
    assert_eq!(
        h.state().explorer_selection(),
        Some(("c1".to_string(), NodePath::root())),
        "the explorer selects the connection"
    );
}

#[test]
fn chip_switches_and_disconnects() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    open_orders(&mut h);
    // The toolbar chip is the topmost "fake-conn".
    let chip = |h: &Harness<'static, SqlrApp>| {
        h.get_all_by_label("fake-conn")
            .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
            .unwrap()
            .accesskit_node()
            .id()
    };
    let id = chip(&h);
    h.get_all_by_label("fake-conn")
        .find(|n| n.accesskit_node().id() == id)
        .unwrap()
        .click();
    h.run_steps(2);
    h.get_by_label("CONNECTIONS");
    h.get_by_label("Disconnect").click();
    step_until(&mut h, "the connection closes", |h| {
        h.query_by_label("Offline").is_some()
    });
}

#[test]
fn chrome_fits_small_window() {
    let dir = tempfile::tempdir().unwrap();
    let (mut deps, _) = deps_with(dir.path(), FakeScript::default());
    deps.settings.ui_scale = 1.5;
    // Production commits manually: the run below leaves a transaction line.
    let mut cfg = deps.repo.list().unwrap().remove(0);
    cfg.profile = Some("production".into());
    deps.repo.save(&cfg, &Secrets::default()).unwrap();
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((800.0, 600.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(5);
    assert_eq!(h.ctx.zoom_factor(), 1.5);
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    let labels = [
        "No connection",
        "Run",
        "Run script",
        "Stop",
        "Refresh",
        "Command palette",
        "Toggle history panel",
        "Toggle value panel",
        "Settings",
    ];
    let mut rects = Vec::new();
    for label in labels {
        let rect = h.get_by_label(label).rect();
        assert!(
            window.contains_rect(rect),
            "{label} at {rect:?} leaves the window"
        );
        rects.push((label, rect));
    }
    // The menus fold into one button, or stay whole.
    let menu = h
        .query_by_label("Menu")
        .or_else(|| h.query_by_label("File"))
        .expect("a menu")
        .rect();
    rects.push(("menu", menu));
    for (i, (a, ra)) in rects.iter().enumerate() {
        for (b, rb) in &rects[i + 1..] {
            assert!(
                ra.intersect(*rb).area() < 1.0,
                "{a} {ra:?} overlaps {b} {rb:?}"
            );
        }
    }

    // The status bar with a connection, a transaction, a result and the fetch controls.
    // The explorer and the value panel at their default widths leave no room for the
    // editor in a 533-point-wide window: hide the value panel.
    h.get_by_label("Toggle value panel").click();
    h.run_steps(2);
    open_orders(&mut h);
    replace_text(&mut h, "select 1;\nsele");
    h.get_by_label_contains("Transaction: 1 statement");
    let mut rects = Vec::new();
    for label in ["Connected", "Ln 2, Col 5", "UTF-8"] {
        let rect = h.get_by_label(label).rect();
        assert!(
            window.contains_rect(rect),
            "{label} at {rect:?} leaves the window"
        );
        rects.push((label.to_string(), rect));
    }
    // Whatever else the bar shows stays inside it and clear of those.
    for label in ["Fetch all", "Auto-commit"] {
        if let Some(node) = h.query_by_label(label) {
            rects.push((label.to_string(), node.rect()));
        }
    }
    if let Some(node) = h.query_by_label_contains("Transaction:") {
        rects.push(("transaction".into(), node.rect()));
    }
    if let Some(node) = h.query_by_label_contains("rows") {
        rects.push(("rows".into(), node.rect()));
    }
    for (label, rect) in &rects {
        assert!(
            window.contains_rect(*rect),
            "{label} at {rect:?} leaves the window"
        );
    }
    for (i, (a, ra)) in rects.iter().enumerate() {
        for (b, rb) in &rects[i + 1..] {
            assert!(
                ra.intersect(*rb).area() < 1.0,
                "{a} {ra:?} overlaps {b} {rb:?}"
            );
        }
    }
}

#[test]
fn folded_transaction_controls_still_switch_the_commit_mode() {
    let dir = tempfile::tempdir().unwrap();
    let (mut deps, _) = deps_with(dir.path(), FakeScript::default());
    deps.settings.ui_scale = 1.5;
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((800.0, 600.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(5);
    h.get_by_label("Toggle value panel").click();
    h.run_steps(2);
    open_orders(&mut h);
    assert!(
        h.query_by_label("Commit").is_none(),
        "the controls are folded"
    );
    h.get_by_label("More tools").click();
    h.run_steps(3);
    h.get_by_label("Manual").click();
    h.run_steps(3);
    if h.query_by_label("Manual").is_none() {
        h.get_by_label("More tools").click();
        h.run_steps(3);
    }
    step_until(&mut h, "the manual commit mode", |h| {
        h.get_by_label("Manual").accesskit_node().toggled() == Some(egui::accesskit::Toggled::True)
    });
}

#[test]
fn chip_selects_another_connection_for_the_tab() {
    let dir = tempfile::tempdir().unwrap();
    let (deps, _) = deps_with(dir.path(), FakeScript::default());
    let mut other = deps.repo.list().unwrap().remove(0);
    other.id = "c2".into();
    other.name = "other-conn".into();
    deps.repo.save(&other, &Secrets::default()).unwrap();
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    open_orders(&mut h);
    let chip = h
        .get_all_by_label("fake-conn")
        .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .unwrap()
        .accesskit_node()
        .id();
    h.get_all_by_label("fake-conn")
        .find(|n| n.accesskit_node().id() == chip)
        .unwrap()
        .click();
    h.run_steps(2);
    h.get_by_label("CONNECTIONS");
    // The popup's row, below the explorer's.
    h.get_all_by_label("other-conn")
        .max_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
        .unwrap()
        .click();
    h.run_steps(3);
    let conns = h.state().tab_conns();
    assert!(conns.contains(&Some("c2".to_string())), "{conns:?}");
}

/// A fake driver whose `orders` table has a definition, so its DDL can be written.
fn ddl_script() -> FakeScript {
    let mut orders = sqlr_core::TableDef::new(sqlr_core::TableRef {
        database: None,
        schema: None,
        name: "orders".into(),
    });
    orders.columns = ["id", "total"]
        .iter()
        .map(|c| sqlr_core::ColumnDef {
            name: c.to_string(),
            type_name: "integer".into(),
            nullable: true,
            ..Default::default()
        })
        .collect();
    FakeScript {
        editing: Some(sqlr_core::ObjectEditing {
            create: vec![sqlr_core::ObjectKind::Table],
            ..Default::default()
        }),
        objects: vec![sqlr_core::ObjectDef::Table(orders)],
        ..Default::default()
    }
}

#[test]
fn open_ddl_tab() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = harness_with_script(&dir, ddl_script());
    expand_to_orders(&mut h);
    context_menu(&mut h, "orders", "Open DDL");
    step_until(&mut h, "the DDL", |h| h.state().ddl_text().is_some());
    assert!(
        h.state().tab_titles().contains(&"DDL · orders".to_string()),
        "{:?}",
        h.state().tab_titles()
    );
    let text = h.state().ddl_text().unwrap();
    assert!(
        text.contains("CREATE TABLE orders (id integer, total integer)"),
        "{text}"
    );

    // Opening it again focuses the same tab.
    let tabs = h.state().tab_titles().len();
    context_menu(&mut h, "orders", "Open DDL");
    h.run_steps(3);
    assert_eq!(h.state().tab_titles().len(), tabs);

    // "Open in new editor tab" copies it into an editor tab.
    h.get_by_label("Open in new editor tab").click();
    h.run_steps(3);
    assert_eq!(h.state_mut().active_text().as_deref(), Some(text.as_str()));
}

#[test]
fn select_100_rows_and_copy_names() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    expand_to_orders(&mut h);
    context_menu(&mut h, "orders", "Select 100 rows");
    step_until(&mut h, "the first page", |h| {
        h.query_by_label_contains("100 rows").is_some()
            || h.query_by_label_contains("500 rows").is_some()
    });
    let sql = h.state_mut().active_text().unwrap();
    assert!(sql.contains("LIMIT 100"), "{sql}");

    context_menu(&mut h, "orders", "Copy qualified name");
    assert!(
        h.state()
            .toast_texts()
            .iter()
            .any(|t| t.contains("Copied s.orders")),
        "{:?}",
        h.state().toast_texts()
    );
}

#[test]
fn failed_connection_retries_inline() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = harness_with_script(
        &dir,
        FakeScript {
            connect_error: Some(DriverError::new(
                ErrorKind::Connection,
                "connection refused\nis the server running?",
            )),
            ..Default::default()
        },
    );
    h.get_by_label("Expand fake-conn").click();
    wait_for_label(&mut h, "Could not connect: connection refused");
    let before = driver.connects().len();
    h.get_by_label("Retry").click();
    step_until(&mut h, "the second connect", |_| {
        driver.connects().len() > before
    });
}

#[test]
fn connection_menu_opens_an_editor_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    let tabs = h.state().tab_titles().len();
    context_menu(&mut h, "fake-conn", "New editor tab");
    assert_eq!(h.state().tab_titles().len(), tabs + 1);
    assert_eq!(h.state().tab_conns().last(), Some(&Some("c1".to_string())));
}

/// Renders `docs/media/screenshots/explorer-{dark,light}.png` (folder counts, the table
/// menu): `cargo test -p sqlr-app --test smoke explorer_screenshots -- --ignored`.
#[test]
#[ignore = "writes screenshots; needs a wgpu adapter"]
fn explorer_screenshots() {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/media/screenshots");
    std::fs::create_dir_all(&out).unwrap();
    for (name, pref) in [("dark", ThemePref::Dark), ("light", ThemePref::Light)] {
        let dir = tempfile::tempdir().unwrap();
        let (mut deps, _) = deps_with(dir.path(), ddl_script());
        deps.settings.theme = pref;
        let app = SqlrApp::new(deps, &egui::Context::default());
        let mut h = Harness::builder()
            .with_size((1400.0, 860.0))
            .with_step_dt(0.01)
            .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
        sqlr_ui::theme::install_fonts(&h.ctx, &[]);
        h.run_steps(2);
        expand_to_orders(&mut h);
        context_menu(&mut h, "orders", "Open DDL");
        step_until(&mut h, "the DDL", |h| h.state().ddl_text().is_some());
        h.run_steps(100);
        h.get_all_by_label("orders")
            .min_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
            .unwrap()
            .click_secondary();
        h.run_steps(60);
        let image = h.render().expect("render");
        image
            .save(out.join(format!("explorer-{name}.png")))
            .unwrap();
    }
}

fn history_entry(conn: &str, sql: &str, database: Option<&str>) -> HistoryEntry {
    HistoryEntry {
        id: 0,
        conn_id: conn.into(),
        database: database.map(Into::into),
        sql: sql.into(),
        started_at_ms: 1_700_000_000_000,
        duration_ms: 5,
        outcome: HistoryOutcome::Rows(1),
    }
}

/// Opens the right panel and switches it to the history list.
fn open_history(h: &mut Harness<'static, SqlrApp>) {
    h.get_by_label("View").click();
    h.run_steps(2);
    h.get_by_label("History").click();
    h.run_steps(2);
}

#[test]
fn history_open_restores_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    db.add_history(&history_entry("c1", "select 42", Some("analytics")))
        .unwrap();
    let (deps, _) = deps_with_db(dir.path(), FakeScript::default(), Some(db));
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    open_history(&mut h);
    wait_for_label(&mut h, "select 42");
    double_click(&mut h, "select 42");
    h.run_steps(2);
    assert_eq!(h.state().tab_conns().last(), Some(&Some("c1".to_string())));
    assert_eq!(
        h.state().tab_databases().last(),
        Some(&Some("analytics".to_string()))
    );
}

#[test]
fn history_open_missing_connection() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    db.add_history(&history_entry("gone", "select 7", Some("analytics")))
        .unwrap();
    let (deps, _) = deps_with_db(dir.path(), FakeScript::default(), Some(db));
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    open_history(&mut h);
    wait_for_label(&mut h, "select 7");
    double_click(&mut h, "select 7");
    h.run_steps(2);
    assert_eq!(h.state().tab_conns().last(), Some(&None), "opened unbound");
    assert!(
        h.state().toast_texts().iter().any(|t| t.contains("gone")),
        "{:?}",
        h.state().toast_texts()
    );
}

#[test]
fn history_clear_asks_first() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    db.add_history(&history_entry("c1", "select 42", None))
        .unwrap();
    let (deps, _) = deps_with_db(dir.path(), FakeScript::default(), Some(db));
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    open_history(&mut h);
    wait_for_label(&mut h, "select 42");
    h.get_by_label("Clear history").click_accesskit();
    h.run_steps(2);
    h.get_by_label_contains("Clear the query history");
    h.run_steps(2);
    h.get_by_label("Delete").click_accesskit();
    step_until(&mut h, "the history clears", |h| {
        h.query_by_label("select 42").is_none()
    });
}

#[test]
fn settings_ui_scale_applies() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    press(&mut h, Modifiers::COMMAND, Key::Comma);
    h.run_steps(2);
    h.get_by_label("150%").click();
    h.run_steps(2);
    h.get_by_label("Save").click_accesskit();
    step_until(&mut h, "the UI scale applies", |h| {
        h.ctx.zoom_factor() == 1.5
    });
    assert_eq!(h.ctx.zoom_factor(), 1.5);
}

#[test]
fn view_menu_toggles_soft_wrap() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    assert!(!h.state().soft_wrap_enabled());
    h.get_by_label("View").click();
    h.run_steps(2);
    h.get_by_label("Soft wrap").click();
    h.run_steps(2);
    assert!(h.state().soft_wrap_enabled());
}

#[test]
fn settings_open_folder_spawns_opener() {
    use std::sync::Mutex;

    let dir = tempfile::tempdir().unwrap();
    let (deps, _) = deps_with(dir.path(), FakeScript::default());
    let config_dir = deps.config_dir.clone();
    let app = SqlrApp::new(deps, &egui::Context::default());
    let seen: Arc<Mutex<Vec<std::path::PathBuf>>> = Arc::default();
    let app = {
        let seen = seen.clone();
        let mut app = app;
        app.set_folder_opener(move |path| seen.lock().unwrap().push(path.to_path_buf()));
        app
    };
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    press(&mut h, Modifiers::COMMAND, Key::Comma);
    h.run_steps(2);
    h.get_by_label("Open settings folder").click();
    h.run_steps(2);
    assert_eq!(*seen.lock().unwrap(), [config_dir]);
}

// ----- Drivers that are not installed -----

/// A harness whose saved connection names a driver nobody installed.
fn harness_missing_driver(dir: &tempfile::TempDir) -> Harness<'static, SqlrApp> {
    let (deps, _driver) = deps_with(dir.path(), FakeScript::default());
    let mut cfg = deps.repo.list().unwrap().remove(0);
    cfg.driver = "ghost".into();
    cfg.name = "ghost-conn".into();
    deps.repo.save(&cfg, &Secrets::default()).unwrap();
    deps.engine.configure_drivers(&dir.path().join("data"), 5);
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    h
}

#[test]
fn missing_driver_offers_install() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness_missing_driver(&dir);
    // The connection stays in the tree, with the reason and the way out; it does not vanish
    // or crash, and it does not look like a connection that failed.
    wait_for_label(&mut h, "ghost-conn");
    wait_for_label(&mut h, "Driver ghost is not installed");
    h.get_by_label("driver missing");
    h.get_by_label("Install…").click();
    // The Driver Manager opens, where the driver installs from a local file.
    step_until(&mut h, "the file-install guidance", |h| {
        h.state()
            .toast_texts()
            .iter()
            .any(|t| t.contains("Install the ghost driver from a file"))
    });
    h.get_by_label("Install from file…");
    h.get_by_label("ghost-conn");
}

#[test]
fn connecting_a_connection_without_its_driver_offers_the_install() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness_missing_driver(&dir);
    wait_for_label(&mut h, "Driver ghost is not installed");
    h.get_by_label("ghost-conn").click_secondary();
    h.run_steps(2);
    h.get_by_label("Connect").click();
    step_until(&mut h, "the install prompt", |h| {
        h.state()
            .toast_texts()
            .iter()
            .any(|t| t.contains("Install the ghost driver from a file"))
    });
}

#[test]
fn driver_manager_opens_from_the_palette_and_lists_bundled_drivers() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness_missing_driver(&dir);
    wait_for_label(&mut h, "Driver ghost is not installed");
    press(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::P);
    h.run_steps(2);
    h.get_by_label("Command palette")
        .type_text("driver manager");
    h.run_steps(3);
    press(&mut h, Modifiers::NONE, Key::Enter);
    wait_for_label(&mut h, "Search drivers");
    // Bundled drivers are listed, and the only way to add another is a local file.
    h.get_all_by_label("Fake").next().unwrap();
    h.get_by_label("Install from file…");
}

#[test]
fn hana_connection_form_from_manifest() {
    // The real HANA sidecar, packaged and installed like a user would: its form (and its
    // TLS and redirect options) comes from the manifest it announces.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let built = std::process::Command::new(&cargo)
        .args(["build", "-p", "sqlr-driver-hana"])
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(built.success());
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(2).unwrap().to_path_buf(); // .../target/debug
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("hana.tar.gz");
    let packed = std::process::Command::new("sh")
        .arg(root.join("scripts/package-driver.sh"))
        .args(["hana"])
        .arg(&archive)
        .arg("--bin")
        .arg(target.join("sqlr-driver-hana"))
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );

    let (deps, _driver) = deps_with(dir.path(), FakeScript::default());
    deps.engine.configure_drivers(&dir.path().join("data"), 5);
    deps.engine.inspect_manual_install(archive);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !matches!(
        deps.engine.manual_install(),
        sqlr_engine::ManualState::Review(_)
    ) {
        assert!(
            Instant::now() < deadline,
            "{:?}",
            deps.engine.manual_install()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    deps.engine.confirm_manual_install();
    let deadline = Instant::now() + Duration::from_secs(20);
    while deps.engine.drivers().get("hana").is_none() {
        assert!(Instant::now() < deadline, "the driver never registered");
        std::thread::sleep(Duration::from_millis(10));
    }
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(3);
    h.state_mut().open_known_connection_for_tests("sap-hana");
    h.run_steps(5);
    let (driver, _, _) = h.state().conn_dialog_form().expect("the connection dialog");
    assert_eq!(driver, "hana");
    // The form follows the manifest: its defaults are the announced ones.
    let (_, values, _) = h.state().conn_dialog_form().unwrap();
    assert_eq!(
        values.get("port").map(String::as_str),
        Some("30015"),
        "{values:?}"
    );
    assert_eq!(
        values.get("tls").map(String::as_str),
        Some("true"),
        "{values:?}"
    );
    assert_eq!(
        values.get("pin_redirect").map(String::as_str),
        Some("false"),
        "{values:?}"
    );
    // Its groups are tabs: TLS and Advanced hold the options the HANA notes ask for.
    for (tab, field) in [
        ("TLS", "Skip certificate verification"),
        ("Advanced", "Pin the redirect to this host"),
    ] {
        h.get_all_by_label(tab)
            .next()
            .unwrap_or_else(|| panic!("tab {tab}"))
            .click();
        h.run_steps(3);
        assert!(
            h.query_all_by_label(field).next().is_some(),
            "{field} under {tab}"
        );
    }
}

#[test]
fn a_sidecar_connection_gets_its_dialect_once_connected() {
    // The echo driver, installed from a file, announces its dialect only on connect.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(
        std::process::Command::new(&cargo)
            .args(["build", "-p", "sqlr-driver-sdk", "--example", "echo_driver"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    let exe = std::env::current_exe().unwrap();
    let examples = exe.ancestors().nth(2).unwrap().join("examples/echo_driver");
    let dir = tempfile::tempdir().unwrap();
    let pack = dir.path().join("pack");
    std::fs::create_dir_all(&pack).unwrap();
    std::fs::copy(&examples, pack.join("echo")).unwrap();
    std::fs::write(
        pack.join("driver.toml"),
        "id = \"echo\"\nversion = \"1.0.0\"\nexecutable = \"echo\"\n",
    )
    .unwrap();
    let archive = dir.path().join("echo.tar.gz");
    assert!(
        std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&pack)
            .arg(".")
            .status()
            .unwrap()
            .success()
    );

    let (deps, _driver) = deps_with(dir.path(), FakeScript::default());
    let mut cfg = deps.repo.list().unwrap().remove(0);
    cfg.driver = "echo".into();
    deps.repo.save(&cfg, &Secrets::default()).unwrap();
    deps.engine.configure_drivers(&dir.path().join("data"), 5);
    deps.engine.inspect_manual_install(archive);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !matches!(
        deps.engine.manual_install(),
        sqlr_engine::ManualState::Review(_)
    ) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    deps.engine.confirm_manual_install();
    while deps.engine.drivers().get("echo").is_none() {
        assert!(Instant::now() < deadline, "the driver never registered");
        std::thread::sleep(Duration::from_millis(10));
    }
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(3);
    assert!(
        !h.state().has_dialect("c1"),
        "unknown before the first connection"
    );
    h.state_mut().connect_for_tests("c1");
    step_until(&mut h, "the dialect of the connected sidecar", |h| {
        h.state().has_dialect("c1")
    });
}

#[test]
fn remapped_run_reaches_engine_and_old_binding_stops_running() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("keymap.toml"),
        "preset = 'sqlr'\n[bindings]\nrun_statement = ['Alt+R']\n",
    )
    .unwrap();
    let (mut h, driver) = harness_with_driver(&dir);
    open_orders(&mut h);
    replace_text(&mut h, "select 42");
    let before = driver.executed().len();
    press(&mut h, Modifiers::COMMAND, Key::Enter);
    h.run_steps(5);
    assert_eq!(driver.executed().len(), before);
    press(&mut h, Modifiers::ALT, Key::R);
    step_until(&mut h, "remapped run", |_| {
        driver.executed().len() == before + 1
    });
    assert_eq!(
        driver.executed().last().map(String::as_str),
        Some("select 42")
    );
}

#[test]
fn cancel_hung_connect_from_operations() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = harness_with_script(
        &dir,
        FakeScript {
            block_on_connect: true,
            ..Default::default()
        },
    );
    h.state_mut().connect_for_tests("c1");
    step_until(&mut h, "activity chip", |h| {
        h.query_by_label_contains("Operations (1)").is_some()
    });
    h.get_by_label_contains("Operations (1)").click();
    h.run_steps(2);
    h.get_by_label("Cancel").click();
    step_until(&mut h, "cancelled connect", |h| {
        h.query_by_label("Cancelled").is_some()
    });
    // The failed attempt can be retried from the same explorer connection.
    h.state_mut().connect_for_tests("c1");
    step_until(&mut h, "retry", |h| {
        h.query_by_label_contains("Operations (1)").is_some()
    });
}
