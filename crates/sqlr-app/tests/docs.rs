//! Document tabs and index administration in the app, on the fake
//! document driver.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Modifiers;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_core::{ConnectionConfig, DocValue, Node, NodeKind, NodePath, Secrets};
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
use sqlr_testkit::{FakeDocDriver, FakeDocScript};

const TIMEOUT: Duration = Duration::from_secs(5);

fn node(parent: &NodePath, seg: &str, name: &str, kind: NodeKind, children: bool) -> Node {
    Node {
        path: parent.child(seg),
        name: name.into(),
        kind,
        has_children: children,
        detail: None,
    }
}

fn script() -> FakeDocScript {
    let root = NodePath::root();
    let app = root.child("app");
    let colls = app.child("collections");
    let users = colls.child("users");
    let doc = |fields: Vec<(&str, DocValue)>| {
        DocValue::Document(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    };
    FakeDocScript {
        docs: vec![
            doc(vec![
                ("_id", DocValue::Int32(1)),
                ("name", DocValue::String("ann".into())),
                (
                    "address",
                    doc(vec![("city", DocValue::String("Rome".into()))]),
                ),
            ]),
            doc(vec![
                ("_id", DocValue::Int32(2)),
                ("name", DocValue::String("bob".into())),
            ]),
        ],
        tree: vec![
            node(&root, "app", "app", NodeKind::Database, true),
            node(&app, "collections", "Collections", NodeKind::Folder, true),
            node(&colls, "users", "users", NodeKind::Table, true),
            node(&users, "email_1", "email_1", NodeKind::Index, false),
        ],
        ..Default::default()
    }
}

fn app(dir: &std::path::Path) -> (Harness<'static, SqlrApp>, Arc<FakeDocDriver>) {
    let driver = FakeDocDriver::new(script());
    let mut drivers = DriverRegistry::default();
    drivers.register(driver.clone());
    let engine = Engine::new(drivers, Arc::new(|| {}));
    let repo = ConnectionRepo::open(dir, Box::new(MemorySecretStore::default())).unwrap();
    repo.save(
        &ConnectionConfig {
            id: "m1".into(),
            name: "mongo-conn".into(),
            driver: "fakedoc".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        },
        &Secrets::default(),
    )
    .unwrap();
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
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    (h, driver)
}

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
                modifiers: Modifiers::NONE,
            });
        }
        h.step();
    }
}

fn context_menu(h: &mut Harness<'static, SqlrApp>, label: &str, submenu: Option<&str>, item: &str) {
    h.run_steps(100);
    h.get_by_label(label).click_secondary();
    h.run_steps(2);
    if let Some(sub) = submenu {
        h.get_by_label(&format!("{sub} ⏵")).hover();
        h.run_steps(30);
    }
    wait_for_label(h, item);
    h.get_by_label(item).click();
    h.run_steps(2);
}

fn expand_to_users(h: &mut Harness<'static, SqlrApp>) {
    h.get_by_label("Expand mongo-conn").click();
    wait_for_label(h, "Expand app");
    h.get_by_label("Expand app").click();
    wait_for_label(h, "Expand Collections");
    h.get_by_label("Expand Collections").click();
    wait_for_label(h, "users");
}

#[test]
fn mongo_tab_find_and_switch_views() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = app(dir.path());
    expand_to_users(&mut h);
    double_click(&mut h, "users");
    step_until(&mut h, "the document tab", |h| {
        h.state().tab_titles().iter().any(|t| t == "users (app)")
    });
    wait_for_label(&mut h, "2 documents");
    assert_eq!(
        driver.runs(),
        [(
            Some("app".to_string()),
            "db.users.find({}).limit(500)".to_string()
        )]
    );
    // Table is the default view; the tree shows the documents collapsed.
    h.get_by_label("Table");
    h.get_by_label("Tree").click();
    wait_for_label(&mut h, "Expand document 1");
    h.get_by_label("Expand document 1").click();
    wait_for_label(&mut h, "address");
    // The JSON view shows the selected document.
    h.get_by_label("JSON").click();
    step_until(&mut h, "the JSON", |h| {
        h.query_by_label_contains("\"Rome\"").is_some()
    });
    h.get_by_label("Messages").click();
    step_until(&mut h, "the outcome message", |h| {
        h.query_by_label_contains("2 documents").is_some()
    });
    // The view is remembered per tab.
    h.get_by_label("Result").click();
    step_until(&mut h, "the JSON again", |h| {
        h.query_by_label_contains("\"Rome\"").is_some()
    });
}

#[test]
fn create_and_drop_mongo_index() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = app(dir.path());
    expand_to_users(&mut h);
    context_menu(&mut h, "users", Some("New"), "Index");
    wait_for_label(&mut h, "New index on users");
    h.get_by_role_and_label(Role::TextInput, "Key 1").focus();
    h.run_steps(2);
    h.event(egui::Event::Text("email".into()));
    h.run_steps(2);
    h.get_by_label("Unique").click();
    h.run_steps(2);
    h.get_by_label("Create").click();
    step_until(&mut h, "the createIndex run", |_| !driver.runs().is_empty());
    assert_eq!(
        driver.runs(),
        [(
            Some("app".to_string()),
            "db.getCollection(\"users\").createIndex({\"email\": 1}, {\"unique\": true})"
                .to_string()
        )]
    );
    step_until(&mut h, "the dialog to close", |h| {
        h.query_by_label("New index on users").is_none()
    });

    wait_for_label(&mut h, "Expand users");
    h.get_by_label("Expand users").click();
    wait_for_label(&mut h, "email_1");
    context_menu(&mut h, "email_1", None, "Drop…");
    wait_for_label(&mut h, "Drop index email_1 of users?");
    h.get_by_label("Drop").click();
    step_until(&mut h, "the dropIndex run", |_| driver.runs().len() == 2);
    assert_eq!(
        driver.runs()[1],
        (
            Some("app".to_string()),
            "db.getCollection(\"users\").dropIndex(\"email_1\")".to_string()
        )
    );
}

fn open_users_json(h: &mut Harness<'static, SqlrApp>) {
    expand_to_users(h);
    double_click(h, "users");
    wait_for_label(h, "2 documents");
    h.get_by_label("JSON").click();
    step_until(h, "the JSON", |h| {
        h.query_by_label_contains("\"Rome\"").is_some()
    });
}

#[test]
fn save_from_the_json_view_then_delete() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = app(dir.path());
    open_users_json(&mut h);
    h.get_by_label("Edit document").click();
    wait_for_label(&mut h, "Editing document 1");
    let runs = driver.runs().len();
    h.get_by_label("Save").click();
    // Saved, the editor closes and the result is read again.
    step_until(&mut h, "the save", |h| {
        h.query_by_label("Editing document 1").is_none()
    });
    step_until(&mut h, "the re-read", |_| driver.runs().len() > runs);
    assert_eq!(driver.docs().len(), 2);

    h.get_by_label("Tree").click();
    wait_for_label(&mut h, "Expand document 2");
    h.run_steps(50);
    h.get_by_label_contains("(2)").click_secondary();
    h.run_steps(3);
    wait_for_label(&mut h, "Delete document…");
    h.get_by_label("Delete document…").click();
    wait_for_label(&mut h, "Delete 1 document?");
    // Let the modal settle where it is shown before clicking in it.
    h.run_steps(5);
    h.get_by_label("Delete").click();
    step_until(&mut h, "the delete", |_| driver.docs().len() == 1);
}

#[test]
fn a_failed_save_keeps_the_text_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = app(dir.path());
    open_users_json(&mut h);
    h.get_by_label("Edit document").click();
    wait_for_label(&mut h, "Editing document 1");
    // Someone else deletes the documents meanwhile.
    driver.set_docs(Vec::new());
    let runs = driver.runs().len();
    h.get_by_label("Save").click();
    step_until(&mut h, "the failure", |h| {
        h.query_by_label_contains("no longer exists").is_some()
    });
    h.run_steps(20);
    h.get_by_label("Editing document 1");
    h.get_by_label_contains("no longer exists");
    assert_eq!(
        driver.runs().len(),
        runs,
        "a failed save reads nothing again"
    );
}

#[test]
fn palette_opens_collections_and_creates_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = app(dir.path());
    expand_to_users(&mut h);
    // ⌘P lists the collections the explorer knows.
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::P);
    h.run_steps(2);
    wait_for_label(&mut h, "app.users");
    h.event(egui::Event::Text("users".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    step_until(&mut h, "the document tab", |h| {
        h.state().tab_titles().iter().any(|t| t == "users (app)")
    });
    step_until(&mut h, "the find", |_| !driver.runs().is_empty());
    // ⌘⇧P offers "New index" on the collection selected in the explorer.
    h.get_by_label("users").click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, egui::Key::P);
    h.run_steps(2);
    h.event(egui::Event::Text("new index".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    wait_for_label(&mut h, "New index on users");
}

#[path = "support/visual.rs"]
mod visual;

#[test]
#[ignore = "visual audit; needs a GPU adapter"]
fn documents_visual_audit() {
    for light in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (mut h, _) = app(dir.path());
        visual::set_theme(&mut h, light);
        expand_to_users(&mut h);
        double_click(&mut h, "users");
        wait_for_label(&mut h, "2 documents");
        visual::capture(&mut h, "documents-table", light);
        h.get_by_label("Tree").click();
        wait_for_label(&mut h, "Expand document 1");
        h.get_by_label("Expand document 1").click();
        h.run_steps(3);
        visual::capture(&mut h, "documents-tree", light);
        h.get_by_label("JSON").click();
        h.run_steps(5);
        visual::capture(&mut h, "documents-json", light);
        context_menu(&mut h, "users", Some("New"), "Index");
        wait_for_label(&mut h, "New index on users");
        visual::capture(&mut h, "documents-new-index", light);
    }
}

#[test]
fn document_results_have_one_inspector_and_respect_the_value_panel_toggle() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = app(dir.path());
    expand_to_users(&mut h);
    double_click(&mut h, "users");
    wait_for_label(&mut h, "2 documents");
    h.run_steps(5);
    let inspectors = || egui_kittest::kittest::By::new().label("No cell selected");
    assert_eq!(
        h.query_all(inspectors()).count(),
        1,
        "Document results need one inspector"
    );
    h.get_by_label("View").click();
    h.run_steps(3);
    h.get_by_label("Value panel").click();
    h.key_press(egui::Key::Escape);
    h.run_steps(5);
    assert_eq!(
        h.query_all(inspectors()).count(),
        0,
        "Value panel toggle must hide the document inspector"
    );
}
