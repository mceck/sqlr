//! Object editors in the app, on the fake driver: opening an editor
//! from the explorer, applying (and failing to), creating, dropping, Generate SQL.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_core::{
    ColumnDef, ConnectionConfig, ConstraintDef, Node, NodeKind, NodePath, ObjectDef, ObjectEditing,
    ObjectKind, Secrets, TableDef, TableRef,
};
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
use sqlr_testkit::{FakeDriver, FakeScript};

const TIMEOUT: Duration = Duration::from_secs(5);

fn node(parent: &NodePath, name: &str, kind: NodeKind) -> Node {
    Node {
        path: parent.child(name),
        name: name.into(),
        kind,
        has_children: kind != NodeKind::Table,
        detail: None,
    }
}

fn orders() -> TableDef {
    let mut def = TableDef::new(TableRef {
        database: None,
        schema: None,
        name: "orders".into(),
    });
    def.columns = ["id", "total"]
        .iter()
        .map(|c| ColumnDef {
            name: c.to_string(),
            type_name: "integer".into(),
            nullable: *c != "id",
            ..Default::default()
        })
        .collect();
    def.constraints.push(ConstraintDef::PrimaryKey {
        name: Some("orders_pkey".into()),
        columns: vec!["id".into()],
    });
    def
}

fn harness(
    script: FakeScript,
) -> (
    Harness<'static, SqlrApp>,
    Arc<FakeDriver>,
    tempfile::TempDir,
) {
    harness_with(script, sqlr_ui::theme::ThemePref::System, None)
}

fn harness_themed(
    script: FakeScript,
    theme: sqlr_ui::theme::ThemePref,
) -> (
    Harness<'static, SqlrApp>,
    Arc<FakeDriver>,
    tempfile::TempDir,
) {
    harness_with(script, theme, None)
}

/// A harness whose connection uses the connection profile `profile` (default: development).
fn harness_with(
    script: FakeScript,
    theme: sqlr_ui::theme::ThemePref,
    profile: Option<&str>,
) -> (
    Harness<'static, SqlrApp>,
    Arc<FakeDriver>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let root = NodePath::root();
    let s = node(&root, "s", NodeKind::Schema);
    let tables = node(&s.path, "tables", NodeKind::Folder);
    let orders_node = node(&tables.path, "orders", NodeKind::Table);
    let default_editing = ObjectEditing {
        create: vec![ObjectKind::Table, ObjectKind::View, ObjectKind::Index],
        alter: vec![ObjectKind::Table],
        drop: vec![ObjectKind::Table],
        rename: vec![ObjectKind::Table],
        transactional_ddl: true,
        column_types: vec!["integer".into(), "text".into()],
        comments: false,
        column_order: false,
    };
    let driver = FakeDriver::new(FakeScript {
        editing: script.editing.clone().or(Some(default_editing)),
        objects: vec![ObjectDef::Table(orders())],
        tables_folder: Some(tables.path.clone()),
        tree: vec![s, tables, orders_node],
        default_schemas: vec!["s".into()],
        catalog: vec![sqlr_core::SchemaCatalog {
            schema: "s".into(),
            relations: vec![sqlr_core::CatalogRelation {
                name: "orders".into(),
                kind: sqlr_core::RelationKind::Table,
                columns: vec![sqlr_core::CatalogColumn {
                    name: "id".into(),
                    type_name: "integer".into(),
                }],
            }],
        }],
        ..script
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(driver.clone());
    let engine = Engine::new(drivers, Arc::new(|| {}));
    let repo = ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
    let cfg = ConnectionConfig {
        id: "c1".into(),
        name: "fake-conn".into(),
        driver: "fake".into(),
        group: None,
        color: None,
        values: [("host".to_string(), "fake-host".to_string())].into(),
        profile: profile.map(String::from),
        save_secrets: true,
    };
    repo.save(&cfg, &Secrets::default()).unwrap();
    if profile == Some("ro") {
        sqlr_store::ProfileRepo::open(dir.path())
            .save(&sqlr_core::ConnectionProfile {
                id: "ro".into(),
                name: "Read only".into(),
                read_only: true,
                ..sqlr_core::ConnectionProfile::development()
            })
            .unwrap();
    }
    if profile == Some("manual") {
        // Manual commit mode without the production profile's confirmations.
        sqlr_store::ProfileRepo::open(dir.path())
            .save(&sqlr_core::ConnectionProfile {
                id: "manual".into(),
                name: "Manual".into(),
                commit_mode: sqlr_core::CommitMode::Manual,
                confirm_destructive: false,
                ..sqlr_core::ConnectionProfile::development()
            })
            .unwrap();
    }
    let deps = AppDeps {
        engine,
        repo,
        settings: Settings {
            theme,
            ..Settings::default()
        },
        config_dir: dir.path().to_path_buf(),
        data_dir: None,
        startup_notice: None,
        storage_warning: None,
    };
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    (h, driver, dir)
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

fn expand_to_orders(h: &mut Harness<'static, SqlrApp>) {
    h.get_by_label("Expand fake-conn").click();
    wait_for_label(h, "Expand s");
    h.get_by_label("Expand s").click();
    wait_for_label(h, "Expand tables");
    h.get_by_label("Expand tables").click();
    wait_for_label(h, "orders");
}

/// Opens `label`'s context menu and clicks `item`, through `submenu` when given.
fn context_menu(h: &mut Harness<'static, SqlrApp>, label: &str, submenu: Option<&str>, item: &str) {
    h.run_steps(100);
    // The explorer row: the leftmost node of that name (the toolbar chip and the editor's
    // breadcrumb name the tab's connection too).
    h.get_all_by_label(label)
        .min_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
        .unwrap_or_else(|| panic!("no node labeled {label}"))
        .click_secondary();
    h.run_steps(2);
    if let Some(sub) = submenu {
        h.get_by_label(&format!("{sub} ⏵")).hover();
        h.run_steps(30);
    }
    wait_for_label(h, item);
    h.get_by_label(item).click();
    h.run_steps(2);
}

fn field(value: &str) -> By<'_> {
    By::new().role(Role::TextInput).value(value)
}

fn open_orders_editor(h: &mut Harness<'static, SqlrApp>) {
    expand_to_orders(h);
    context_menu(h, "orders", None, "Open editor");
    step_until(h, "the definition", |h| h.query(field("total")).is_some());
}

#[test]
fn open_table_editor_from_explorer() {
    let (mut h, driver, _dir) = harness(FakeScript::default());
    open_orders_editor(&mut h);
    assert_eq!(h.state().tab_titles(), ["Query 1", "orders"]);
    h.get_by_label("Columns");
    h.get_by_label("No pending changes");
    // Opening it again focuses the same tab.
    context_menu(&mut h, "orders", None, "Open editor");
    assert_eq!(h.state().tab_titles(), ["Query 1", "orders"]);
    assert!(driver.executed().is_empty(), "{:?}", driver.executed());
}

#[test]
fn apply_failure_keeps_pending() {
    let (mut h, driver, _dir) = harness(FakeScript {
        fail_matching: Some("ADD COLUMN".into()),
        ..Default::default()
    });
    open_orders_editor(&mut h);
    h.get_by_label("Add column").click();
    h.run_steps(2);
    wait_for_label(&mut h, "1 pending change");
    h.get_by_label("Apply").click();
    step_until(&mut h, "the failure", |h| {
        h.query_by_label_contains("Statement 1 of 1 failed")
            .is_some()
    });
    h.get_by_label_contains("The transaction was rolled back.");
    h.get_by_label("1 pending change");
    assert_eq!(
        driver.executed(),
        [
            "BEGIN",
            "ALTER TABLE orders ADD COLUMN column1 integer",
            "ROLLBACK"
        ]
    );
}

#[test]
fn create_table_dialog_flow() {
    let (mut h, driver, _dir) = harness(FakeScript::default());
    expand_to_orders(&mut h);
    context_menu(&mut h, "tables", Some("New"), "Table");
    wait_for_label(&mut h, "New table");
    h.run_steps(2);
    h.event(egui::Event::Text("customers".into()));
    h.run_steps(2);
    h.get_by_label("Create").click();
    step_until(&mut h, "the object tab", |h| {
        h.state().tab_titles() == ["Query 1", "customers"]
    });
    wait_for_label(&mut h, "new");
    h.get_by_label("Add column").click();
    h.run_steps(2);
    h.get_by_label("Add column").click();
    h.run_steps(2);
    let apply = h.get_by_label("Apply");
    assert!(!apply.accesskit_node().is_disabled());
    apply.click();
    step_until(&mut h, "the new table in the explorer", |h| {
        h.query_by_label("customers").is_some() && h.query_by_label("new").is_none()
    });
    assert_eq!(
        driver.executed(),
        [
            "BEGIN",
            "CREATE TABLE customers (column1 integer, column2 integer)",
            "COMMIT"
        ]
    );
    // The tab now edits the created table.
    wait_for_label(&mut h, "No pending changes");
    h.get(field("column2"));
}

#[test]
fn drop_table_confirms() {
    let (mut h, driver, _dir) = harness(FakeScript::default());
    expand_to_orders(&mut h);
    context_menu(&mut h, "orders", None, "Drop…");
    wait_for_label(&mut h, "Drop table orders?");
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.query_by_label("Drop table orders?").is_none());
    assert!(driver.executed().is_empty());

    context_menu(&mut h, "orders", None, "Drop…");
    wait_for_label(&mut h, "Drop table orders?");
    h.get_by_label("Drop").click();
    step_until(&mut h, "orders to leave the explorer", |h| {
        h.query_by_label("orders").is_none()
    });
    assert_eq!(driver.executed(), ["BEGIN", "DROP TABLE orders", "COMMIT"]);
    wait_for_label(&mut h, "Dropped table orders");
}

#[test]
fn generate_sql_opens_tab() {
    let (mut h, driver, _dir) = harness(FakeScript::default());
    expand_to_orders(&mut h);
    context_menu(&mut h, "orders", Some("Generate SQL"), "SELECT");
    step_until(&mut h, "the generated tab", |h| {
        h.state().tab_titles() == ["Query 1", "SELECT orders"]
    });
    assert_eq!(
        h.state_mut().active_text().unwrap(),
        "SELECT id, total\nFROM orders;"
    );
    // It opens, it does not run.
    assert!(driver.executed().is_empty());
}

#[test]
fn rename_in_the_explorer_applies() {
    let (mut h, driver, _dir) = harness(FakeScript::default());
    expand_to_orders(&mut h);
    context_menu(&mut h, "orders", None, "Rename");
    h.run_steps(3);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("purchases".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    step_until(&mut h, "the renamed node", |h| {
        h.query_by_label("purchases").is_some()
    });
    assert_eq!(
        driver.executed(),
        ["BEGIN", "ALTER TABLE orders RENAME TO purchases", "COMMIT"]
    );
}

/// Renders an object editor in both themes to `docs/media/screenshots/` (needs a GPU
/// adapter): `cargo test -p sqlr-app --test objects screenshots -- --ignored`.
#[test]
#[ignore = "writes screenshots; needs a wgpu adapter"]
fn screenshots() {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/media/screenshots");
    std::fs::create_dir_all(&out).unwrap();
    for (name, pref) in [
        ("dark", sqlr_ui::theme::ThemePref::Dark),
        ("light", sqlr_ui::theme::ThemePref::Light),
    ] {
        let (mut h, _, _dir) = harness_themed(FakeScript::default(), pref);
        sqlr_ui::theme::install_fonts(&h.ctx, &[]);
        h.run_steps(2);
        open_orders_editor(&mut h);
        h.get_by_label("Add column").click();
        h.run_steps(60);
        let image = h.render().expect("render");
        image
            .save(out.join(format!("object-editor-{name}.png")))
            .unwrap();
    }
}

#[test]
fn truncate_offers_cascade() {
    let (mut h, driver, _dir) = harness(FakeScript::default());
    expand_to_orders(&mut h);
    context_menu(&mut h, "orders", None, "Truncate…");
    wait_for_label(&mut h, "Delete every row of orders?");
    h.get_by_label("Cascade: also empty the tables that reference it")
        .click();
    h.run_steps(2);
    h.get_by_label("Truncate").click();
    step_until(&mut h, "the truncate", |_| driver.executed().len() == 3);
    assert_eq!(
        driver.executed(),
        ["BEGIN", "TRUNCATE TABLE orders CASCADE", "COMMIT"]
    );
}

/// A text field whose value contains `text`.
fn text_containing(text: &str) -> By<'_> {
    By::new().predicate(move |n| n.value().is_some_and(|v| v.contains(text)))
}

/// The object editor's Refresh button (the toolbar has one too, higher up).
fn editor_refresh(h: &Harness<'static, SqlrApp>) -> egui::Pos2 {
    h.get_all_by_label("Refresh")
        .map(|n| n.rect().center())
        .max_by(|a, b| a.y.total_cmp(&b.y))
        .unwrap()
}

fn click_at(h: &mut Harness<'static, SqlrApp>, pos: egui::Pos2) {
    h.run_steps(100);
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(2);
}

fn editing(transactional: bool) -> ObjectEditing {
    ObjectEditing {
        create: vec![ObjectKind::Table, ObjectKind::View, ObjectKind::Index],
        alter: vec![ObjectKind::Table],
        drop: vec![ObjectKind::Table],
        rename: vec![ObjectKind::Table],
        transactional_ddl: transactional,
        column_types: vec!["integer".into(), "text".into()],
        comments: false,
        column_order: false,
    }
}

#[test]
fn pending_ddl_follows_the_applied_definition() {
    let (mut h, _, _dir) = harness(FakeScript::default());
    open_orders_editor(&mut h);
    h.get_by_label("Add column").click();
    h.run_steps(2);
    h.get_by_label("DDL").click();
    h.run_steps(3);
    assert!(h.query_all(text_containing("ADD COLUMN")).next().is_some());
    h.get_by_label("Apply").click();
    // The toolbar and the DDL sub-tab both say it.
    step_until(&mut h, "the reload", |h| {
        h.query_all_by_label("No pending changes").count() == 2
    });
    assert!(
        h.query_all(text_containing("ADD COLUMN")).next().is_none(),
        "the applied statement is no longer pending"
    );
}

#[test]
fn a_failed_refresh_is_shown() {
    let (mut h, _, _dir) = harness(FakeScript::default());
    open_orders_editor(&mut h);
    // The table goes away behind the editor.
    context_menu(&mut h, "orders", None, "Drop…");
    wait_for_label(&mut h, "Drop table orders?");
    h.get_by_label("Drop").click();
    wait_for_label(&mut h, "Dropped table orders");
    let refresh = editor_refresh(&h);
    click_at(&mut h, refresh);
    step_until(&mut h, "the failure", |h| {
        h.query_by_label("Reloading the definition failed: orders not found")
            .is_some()
    });
    // The definition stays on screen.
    h.get(field("total"));
}

#[test]
fn a_failed_reload_after_a_partial_apply_is_shown() {
    let (mut h, _, _dir) = harness(FakeScript {
        editing: Some(editing(false)),
        fail_matching: Some("ADD COLUMN".into()),
        ..Default::default()
    });
    open_orders_editor(&mut h);
    // Rename the table (applied first), then add a column (fails).
    h.get(field("orders")).click();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("purchases".into()));
    h.run_steps(2);
    h.get_by_label("Add column").click();
    h.run_steps(2);
    h.get_by_label("Apply").click();
    step_until(&mut h, "the reload failure", |h| {
        h.query_by_label_contains("Reloading the definition failed: orders not found")
            .is_some()
    });
    h.get_by_label_contains("Statement 2 of 2 failed");
}

#[test]
fn refresh_with_pending_edits_asks() {
    let (mut h, driver, _dir) = harness(FakeScript::default());
    open_orders_editor(&mut h);
    h.get_by_label("Add column").click();
    h.run_steps(2);
    let refresh = editor_refresh(&h);
    click_at(&mut h, refresh);
    wait_for_label(&mut h, "Discard the pending changes of orders?");
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    h.get_by_label("1 pending change");
    click_at(&mut h, refresh);
    wait_for_label(&mut h, "Discard the pending changes of orders?");
    h.get_by_label("Discard").click();
    step_until(&mut h, "the reload", |h| {
        h.query_by_label("No pending changes").is_some()
    });
    assert!(driver.executed().is_empty());
}

#[test]
fn closing_an_editor_with_pending_edits_asks() {
    let (mut h, _, _dir) = harness(FakeScript::default());
    open_orders_editor(&mut h);
    h.get_by_label("Add column").click();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::W);
    h.run_steps(2);
    wait_for_label(&mut h, "Discard the pending changes of orders?");
    assert_eq!(h.state().tab_titles(), ["Query 1", "orders"]);
    h.get_by_label("Discard").click();
    step_until(&mut h, "the tab to close", |h| {
        h.state().tab_titles() == ["Query 1"]
    });
}

#[test]
fn apply_confirmation_shows_the_full_ddl() {
    let (mut h, driver, _dir) = harness_with(
        FakeScript::default(),
        sqlr_ui::theme::ThemePref::System,
        Some("production"),
    );
    open_orders_editor(&mut h);
    h.get_by_label("Add column").click();
    h.run_steps(2);
    h.get_by_label("Apply").click();
    wait_for_label(&mut h, "Show full DDL");
    // The modal is laid out on its first frame and clickable from the next.
    h.run_steps(5);
    h.get_by_label("Show full DDL").click();
    wait_for_label(&mut h, "Pending DDL");
    assert!(
        h.query_all(text_containing("ADD COLUMN column1"))
            .next()
            .is_some()
    );
    assert!(driver.executed().is_empty());
}

#[test]
fn palette_opens_an_object_editor() {
    let (mut h, _, _dir) = harness(FakeScript::default());
    expand_to_orders(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::P);
    wait_for_label(&mut h, "Edit s.orders");
    h.event(egui::Event::Text("edit s.ord".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    step_until(&mut h, "the editor", |h| h.query(field("total")).is_some());
    assert_eq!(h.state().tab_titles(), ["Query 1", "orders"]);
}

#[test]
fn command_palette_creates_and_edits_objects() {
    let (mut h, _, _dir) = harness(FakeScript::default());
    expand_to_orders(&mut h);
    // "Edit object…" opens the selected explorer node.
    h.get_by_label("orders").click();
    h.run_steps(2);
    h.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::P,
    );
    h.run_steps(2);
    h.event(egui::Event::Text("edit object".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    step_until(&mut h, "the editor", |h| h.query(field("total")).is_some());
    // "New table…" creates on the active tab's connection.
    h.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::P,
    );
    h.run_steps(2);
    h.event(egui::Event::Text("new table".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    wait_for_label(&mut h, "New table");
}

fn double_click_label(h: &mut Harness<'static, SqlrApp>, label: &str) {
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

/// `SELECT`s return `orders` (`id` primary key, `total`) with origins, and the driver edits
/// results: the Data sub-tab can be edited like a result tab.
fn editable_orders() -> FakeScript {
    use sqlr_core::{ColumnKind, ColumnMeta, ColumnOrigin, DisplayHint, TableColumn, TableInfo};
    let table = TableRef {
        database: None,
        schema: None,
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
        result: Some(sqlr_testkit::FakeResult {
            columns: vec![column("id"), column("total")],
            rows: vec![
                vec![sqlr_core::Value::Int(1), sqlr_core::Value::Int(10)],
                vec![sqlr_core::Value::Int(2), sqlr_core::Value::Int(20)],
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
fn data_sub_tab_edits_through_the_changeset() {
    let (mut h, driver, _dir) = harness(editable_orders());
    open_orders_editor(&mut h);
    h.get_by_label("Data").click();
    wait_for_label(&mut h, "20");
    // The tab's run is the object's first page, edited like any result.
    double_click_label(&mut h, "10");
    h.event(egui::Event::Text("15".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    // The changes bar counts the cell; the object editor itself has nothing pending.
    wait_for_label(&mut h, "1 pending change");
    h.get_by_label("No pending changes");
    let apply = h
        .get_all_by_label("Apply")
        .find(|n| !n.accesskit_node().is_disabled())
        .expect("the changes bar's Apply")
        .rect()
        .center();
    click_at(&mut h, apply);
    step_until(&mut h, "the changes to apply", |_| {
        driver.executed().iter().any(|s| s == "COMMIT")
    });
    let executed = driver.executed();
    let begin = executed.iter().position(|s| s == "BEGIN").unwrap();
    assert_eq!(
        executed[begin..begin + 3],
        [
            "BEGIN",
            "UPDATE orders SET total = ? WHERE id = ?",
            "COMMIT"
        ]
    );
    assert_eq!(
        driver.executed_with().last().unwrap().1,
        vec![sqlr_core::Value::Int(15), sqlr_core::Value::Int(1)]
    );
    step_until(&mut h, "the data to reload", |h| {
        h.query_by_label("1 pending change").is_none()
    });
}

/// Edits the Data sub-tab's `10` into `15` (not applied).
fn edit_data_cell(h: &mut Harness<'static, SqlrApp>) {
    h.get_by_label("Data").click();
    wait_for_label(h, "20");
    double_click_label(h, "10");
    h.event(egui::Event::Text("15".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    wait_for_label(h, "1 pending change");
}

/// Clicks the changes bar's Apply (the editor's own Apply is disabled).
fn apply_data_changes(h: &mut Harness<'static, SqlrApp>) {
    let apply = h
        .get_all_by_label("Apply")
        .find(|n| !n.accesskit_node().is_disabled())
        .expect("the changes bar's Apply")
        .rect()
        .center();
    click_at(h, apply);
}

#[test]
fn closing_an_editor_after_a_row_apply_asks_to_end_the_transaction() {
    let (mut h, driver, _dir) = harness_with(
        editable_orders(),
        sqlr_ui::theme::ThemePref::System,
        Some("test"),
    );
    open_orders_editor(&mut h);
    edit_data_cell(&mut h);
    apply_data_changes(&mut h);
    step_until(&mut h, "the row to apply", |_| {
        driver.executed().iter().any(|s| s.starts_with("UPDATE"))
    });
    // Smart mode leaves the transaction open on the tab's session.
    assert!(!driver.executed().iter().any(|s| s == "COMMIT"));
    h.get_by_label("Columns").click();
    h.run_steps(2);
    h.get_by_label("Add column").click();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::W);
    wait_for_label(&mut h, "Discard the pending changes of orders?");
    h.get_by_label("Discard").click();
    wait_for_label(&mut h, "Open transactions on fake-conn (orders).");
    assert_eq!(h.state().tab_titles(), ["Query 1", "orders"]);
    assert!(!driver.executed().iter().any(|s| s == "ROLLBACK"));
}

#[test]
fn closing_an_editor_with_pending_row_edits_asks() {
    let (mut h, driver, _dir) = harness(editable_orders());
    open_orders_editor(&mut h);
    edit_data_cell(&mut h);
    // The object editor itself has nothing pending.
    h.get_by_label("No pending changes");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::W);
    wait_for_label(&mut h, "Discard the pending changes of orders?");
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert_eq!(h.state().tab_titles(), ["Query 1", "orders"]);
    h.get_by_label("1 pending change");
    // Refresh asks too, before dropping the Data page.
    let refresh = editor_refresh(&h);
    click_at(&mut h, refresh);
    wait_for_label(&mut h, "Discard the pending changes of orders?");
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    h.get_by_label("1 pending change");
    assert!(!driver.executed().iter().any(|s| s.starts_with("UPDATE")));
}

fn manual_harness() -> (
    Harness<'static, SqlrApp>,
    Arc<FakeDriver>,
    tempfile::TempDir,
) {
    harness_with(
        editable_orders(),
        sqlr_ui::theme::ThemePref::System,
        Some("manual"),
    )
}

/// The editor's Apply button.
fn editor_apply(h: &Harness<'static, SqlrApp>) -> egui::Pos2 {
    h.get_all_by_label("Apply")
        .map(|n| n.rect().center())
        .min_by(|a, b| a.y.total_cmp(&b.y))
        .unwrap()
}

#[test]
fn manual_mode_editor_applies_after_reading_its_data() {
    let (mut h, driver, _dir) = manual_harness();
    open_orders_editor(&mut h);
    h.get_by_label("Data").click();
    wait_for_label(&mut h, "20");
    // The page read opened no transaction.
    assert!(!driver.executed().iter().any(|s| s == "BEGIN"));
    h.get_by_label("Columns").click();
    h.run_steps(2);
    h.get_by_label("Add column").click();
    h.run_steps(2);
    let apply = editor_apply(&h);
    click_at(&mut h, apply);
    step_until(&mut h, "the column to apply", |_| {
        driver.executed().iter().any(|s| s.contains("ADD COLUMN"))
    });
    step_until(&mut h, "the reload", |h| {
        h.query_by_label("No pending changes").is_some()
    });
}

#[test]
fn manual_mode_editor_apply_is_off_while_another_tab_wrote() {
    let (mut h, driver, _dir) = manual_harness();
    expand_to_orders(&mut h);
    // A data tab on orders writes a row and leaves the transaction open.
    double_click_label(&mut h, "orders");
    step_until(&mut h, "the data tab", |h| {
        h.state().tab_titles() == ["Query 1", "orders"]
    });
    wait_for_label(&mut h, "20");
    double_click_label(&mut h, "10");
    h.event(egui::Event::Text("15".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    wait_for_label(&mut h, "1 pending change");
    apply_data_changes(&mut h);
    step_until(&mut h, "the row to apply", |_| {
        driver.executed().iter().any(|s| s.starts_with("UPDATE"))
    });
    assert!(!driver.executed().iter().any(|s| s == "COMMIT"));
    context_menu(&mut h, "orders", None, "Open editor");
    step_until(&mut h, "the definition", |h| {
        h.query(field("total")).is_some()
    });
    h.get_by_label("Add column").click();
    h.run_steps(2);
    let apply = h
        .get_all_by_label("Apply")
        .min_by(|a, b| a.rect().center().y.total_cmp(&b.rect().center().y))
        .unwrap();
    assert!(apply.accesskit_node().is_disabled(), "Apply is off");
}

#[test]
fn read_only_sessions_offer_no_ddl_in_the_explorer() {
    let (mut h, _, _dir) = harness_with(
        FakeScript::default(),
        sqlr_ui::theme::ThemePref::System,
        Some("ro"),
    );
    // The profiles are read in the background: let them arrive before connecting.
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    expand_to_orders(&mut h);
    h.run_steps(100);
    h.get_by_label("orders").click_secondary();
    h.run_steps(2);
    wait_for_label(&mut h, "Open editor");
    for item in ["Drop…", "Truncate…", "Rename"] {
        assert!(h.query_by_label(item).is_none(), "{item} is offered");
    }
    assert!(h.query_by_label("New ⏵").is_none(), "New is offered");
}

#[test]
fn an_applied_definition_asks_before_dropping_row_edits() {
    let (mut h, driver, _dir) = harness(editable_orders());
    open_orders_editor(&mut h);
    edit_data_cell(&mut h);
    h.get_by_label("Columns").click();
    h.run_steps(2);
    h.get_by_label("Add column").click();
    h.run_steps(2);
    let apply = h
        .get_all_by_label("Apply")
        .find(|n| !n.accesskit_node().is_disabled())
        .expect("the editor's Apply")
        .rect()
        .center();
    click_at(&mut h, apply);
    step_until(&mut h, "the column to apply", |_| {
        driver.executed().iter().any(|s| s.contains("ADD COLUMN"))
    });
    // The Data page is stale, but its row edit is not dropped without asking.
    wait_for_label(&mut h, "Discard the pending changes of orders?");
    h.get_by_label("Discard").click();
    h.run_steps(3);
    h.get_by_label("Data").click();
    step_until(&mut h, "the page without its edit", |h| {
        h.query_by_label("10").is_some() && h.query_by_label("1 pending change").is_none()
    });
}
