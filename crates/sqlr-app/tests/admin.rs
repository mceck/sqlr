//! Administration in the app, on the fake driver: the users and roles
//! tab (preview, apply, masked passwords), the sessions tab (confirmations, auto-refresh),
//! the server tab, the menus that open them, and the object editor's Permissions.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::{Role, Toggled};
use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_core::{
    AdminSupport, CatalogColumn, CatalogRelation, ColumnDef, ConnectionConfig, Grant, LockInfo,
    Node, NodeKind, NodePath, ObjectDef, ObjectEditing, ObjectKind, ObjectRef, RelationKind,
    RoleDef, RoleField, SchemaCatalog, Secrets, ServerProperty, SessionInfo, TableDef, TableRef,
    TableStats,
};
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
use sqlr_testkit::{FakeAdmin, FakeDriver, FakeScript};

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

fn orders_ref() -> ObjectRef {
    ObjectRef {
        kind: ObjectKind::Table,
        database: None,
        schema: Some("s".into()),
        name: "orders".into(),
        parent: None,
        signature: None,
    }
}

fn session(id: &str, user: &str, state: &str, query: &str) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        user: user.into(),
        database: Some("app".into()),
        state: state.into(),
        query: Some(query.into()),
        ..Default::default()
    }
}

fn full_admin() -> FakeAdmin {
    FakeAdmin {
        support: AdminSupport {
            roles: true,
            grants: true,
            sessions: true,
            locks: true,
            properties: true,
            statistics: true,
            privileges: vec![(ObjectKind::Table, vec!["SELECT".into(), "INSERT".into()])],
            role_fields: vec![RoleField::Login, RoleField::MemberOf, RoleField::Password],
            hosts: false,
        },
        roles: vec![
            RoleDef {
                name: "reader".into(),
                ..Default::default()
            },
            RoleDef {
                name: "me".into(),
                login: true,
                ..Default::default()
            },
        ],
        grants: vec![Grant {
            grantee: "reader".into(),
            object: orders_ref(),
            privilege: "SELECT".into(),
            grantable: false,
        }],
        // The explorer session's fake connection is number 1.
        sessions: vec![
            session("1", "me", "idle", "SELECT 1"),
            session("7", "other", "active", "select pg_sleep(30)"),
        ],
        locks: vec![LockInfo {
            session: "7".into(),
            blocked_by: vec!["1".into()],
            mode: "ShareLock".into(),
            object: Some("orders".into()),
            granted: false,
        }],
        properties: vec![
            ServerProperty {
                name: "max_connections".into(),
                value: "100".into(),
                category: Some("Connections".into()),
                ..Default::default()
            },
            ServerProperty {
                name: "work_mem".into(),
                value: "4096".into(),
                unit: Some("kB".into()),
                category: Some("Memory".into()),
                ..Default::default()
            },
        ],
        sizes: vec![("app".into(), 20 * 1024 * 1024)],
        stats: vec![TableStats {
            table: TableRef {
                database: None,
                schema: Some("s".into()),
                name: "orders".into(),
            },
            rows_estimate: Some(1234),
            total_bytes: Some(16384),
            table_bytes: Some(8192),
            index_bytes: Some(8192),
            extra: vec![("Last analyze".into(), "yesterday".into())],
        }],
        current_user: Some("me".into()),
    }
}

fn orders() -> TableDef {
    let mut def = TableDef::new(TableRef {
        database: None,
        schema: Some("s".into()),
        name: "orders".into(),
    });
    def.columns = vec![ColumnDef {
        name: "id".into(),
        type_name: "integer".into(),
        ..Default::default()
    }];
    def
}

fn harness(
    admin: FakeAdmin,
) -> (
    Harness<'static, SqlrApp>,
    Arc<FakeDriver>,
    tempfile::TempDir,
) {
    harness_with(admin, false)
}

/// A harness whose connection uses a read-only profile when `read_only`.
fn harness_with(
    admin: FakeAdmin,
    read_only: bool,
) -> (
    Harness<'static, SqlrApp>,
    Arc<FakeDriver>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    if read_only {
        let profile = sqlr_core::ConnectionProfile {
            id: "ro".into(),
            name: "Read only".into(),
            read_only: true,
            ..sqlr_core::ConnectionProfile::development()
        };
        sqlr_store::ProfileRepo::open(dir.path())
            .save(&profile)
            .unwrap();
    }
    let root = NodePath::root();
    let s = node(&root, "s", NodeKind::Schema);
    let tables = node(&s.path, "tables", NodeKind::Folder);
    let orders_node = node(&tables.path, "orders", NodeKind::Table);
    let driver = FakeDriver::new(FakeScript {
        admin: Some(admin),
        editing: Some(ObjectEditing {
            create: vec![ObjectKind::Table],
            alter: vec![ObjectKind::Table],
            drop: vec![ObjectKind::Table],
            rename: vec![ObjectKind::Table],
            transactional_ddl: true,
            column_types: vec!["integer".into()],
            comments: false,
            column_order: false,
        }),
        objects: vec![ObjectDef::Table(orders())],
        tables_folder: Some(tables.path.clone()),
        tree: vec![s, tables, orders_node],
        default_schemas: vec!["s".into()],
        catalog: vec![SchemaCatalog {
            schema: "s".into(),
            relations: vec![CatalogRelation {
                name: "orders".into(),
                kind: RelationKind::Table,
                columns: vec![CatalogColumn {
                    name: "id".into(),
                    type_name: "integer".into(),
                }],
            }],
        }],
        ..Default::default()
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
        profile: read_only.then(|| "ro".to_string()),
        save_secrets: true,
    };
    repo.save(&cfg, &Secrets::default()).unwrap();
    let deps = AppDeps {
        engine,
        repo,
        settings: Settings::default(),
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

/// Selects a session by id through AccessKit: a pointer click would use coordinates from
/// before the "Done" message shifted the table down.
fn select_session(h: &mut Harness<'static, SqlrApp>, id: &str) {
    wait_for_label(h, id);
    h.get_by_label(id).click_accesskit();
    step_until(h, &format!("session {id} to be selected"), |h| {
        h.query_by_label(id)
            .is_some_and(|n| n.accesskit_node().toggled() == Some(Toggled::True))
    });
}

fn connect(h: &mut Harness<'static, SqlrApp>) {
    h.get_by_label("Expand fake-conn").click();
    wait_for_label(h, "connected");
    wait_for_label(h, "Expand s");
}

/// Opens `label`'s context menu and clicks `item`.
fn context_menu(h: &mut Harness<'static, SqlrApp>, label: &str, item: &str) {
    h.run_steps(100);
    // The explorer row: the leftmost node of that name (the toolbar chip and the editor's
    // breadcrumb name the tab's connection too).
    h.get_all_by_label(label)
        .min_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
        .unwrap_or_else(|| panic!("no node labeled {label}"))
        .click_secondary();
    h.run_steps(2);
    wait_for_label(h, item);
    h.get_by_label(item).click();
    h.run_steps(2);
}

/// Clicks `label` in a modal: a modal is laid out on its first frame, and a click in that
/// frame lands on its backdrop.
fn click_in_modal(h: &mut Harness<'static, SqlrApp>, label: &str) {
    wait_for_label(h, label);
    h.run_steps(5);
    h.get_by_label(label).click();
    h.run_steps(2);
}

fn has_value(h: &Harness<'static, SqlrApp>, text: &str) -> bool {
    h.query_all(By::new().predicate(|n| n.value().is_some_and(|v| v.contains(text))))
        .next()
        .is_some()
}

fn open_users(h: &mut Harness<'static, SqlrApp>) {
    connect(h);
    context_menu(h, "fake-conn", "Users and roles");
    step_until(h, "the users tab", |h| {
        h.state().tab_titles() == ["Query 1", "Users and roles (fake-conn)"]
    });
    wait_for_label(h, "reader");
}

#[test]
fn users_tab_preview_apply() {
    let (mut h, driver, _dir) = harness(full_admin());
    open_users(&mut h);
    // The current user is marked.
    h.get_by_label("me (you)");

    h.get_by_label("New role").click();
    h.run_steps(2);
    // The name field has the focus.
    h.event(egui::Event::Text("app".into()));
    h.run_steps(2);
    h.get(By::new().role(Role::PasswordInput)).click();
    h.run_steps(2);
    h.event(egui::Event::Text("s3cret".into()));
    h.run_steps(2);
    // A privilege on a catalog table, in the same apply.
    wait_for_label(&mut h, "SELECT on s.orders");
    h.get_by_label("SELECT on s.orders").click();
    h.run_steps(2);
    wait_for_label(&mut h, "2 pending changes");

    h.get_by_label("Preview").click();
    step_until(&mut h, "the preview", |h| {
        has_value(h, "PASSWORD '********'")
    });
    assert!(has_value(&h, "GRANT SELECT ON s.orders TO app"));
    assert!(!has_value(&h, "s3cret"), "the password is never shown");

    h.get_by_label("Apply").click();
    step_until(&mut h, "the apply", |h| {
        h.query_by_label_contains("2 statements applied").is_some()
    });
    assert_eq!(
        driver.executed(),
        [
            "BEGIN",
            "CREATE ROLE app LOGIN PASSWORD 's3cret'",
            "GRANT SELECT ON s.orders TO app",
            "COMMIT"
        ]
    );
    assert!(!has_value(&h, "s3cret"));
    // The roles load again after the apply.
    step_until(&mut h, "the reload", |_| {
        driver
            .admin_calls()
            .iter()
            .filter(|c| *c == "roles")
            .count()
            >= 2
    });
}

#[test]
fn account_privileges_say_global_ones_are_not_shown() {
    let mut admin = full_admin();
    admin.support.hosts = true;
    let (mut h, _, _dir) = harness(admin);
    connect(&mut h);
    context_menu(&mut h, "fake-conn", "Users and roles");
    wait_for_label(&mut h, "New role");
    h.get_by_label("New role").click();
    wait_for_label(&mut h, "Global privileges (ON *.*) are not listed here");
}

#[test]
fn revoking_and_dropping_ask_first() {
    let (mut h, driver, _dir) = harness(full_admin());
    open_users(&mut h);
    h.get_by_label("reader").click();
    wait_for_label(&mut h, "SELECT on s.orders");
    step_until(&mut h, "the grants", |_| {
        driver.admin_calls().iter().any(|c| c == "grants reader")
    });
    h.run_steps(5);
    // Granted → grantable → none: a revoke, which the development profile confirms.
    h.get_by_label("SELECT on s.orders").click();
    h.run_steps(2);
    h.get_by_label("SELECT on s.orders").click();
    h.run_steps(2);
    wait_for_label(&mut h, "1 pending change");
    h.get_by_label("Apply").click();
    wait_for_label(
        &mut h,
        "This will REVOKE on fake-conn (development profile).",
    );
    click_in_modal(&mut h, "Cancel");
    assert!(driver.executed().is_empty());
    // The revoke stays pending until reverted.
    h.get_by_label("Revert").click();
    h.run_steps(2);
    wait_for_label(&mut h, "No pending changes");

    // Dropping a role always asks; dropping the connected one asks twice.
    h.get_by_label("me (you)").click();
    h.run_steps(5);
    h.get_by_label("Drop role…").click();
    wait_for_label(&mut h, "Drop role me?");
    click_in_modal(&mut h, "Drop role");
    wait_for_label(&mut h, "You are connected as me. Drop it anyway?");
    click_in_modal(&mut h, "Drop it anyway");
    step_until(&mut h, "the drop", |_| {
        driver.executed().contains(&"DROP ROLE me".to_string())
    });
}

/// Selects `reader` and gives it a pending edit (its Login flag).
fn edit_reader(h: &mut Harness<'static, SqlrApp>) {
    h.get_by_label("reader").click();
    wait_for_label(h, "Login");
    h.run_steps(5);
    h.get_by_label("Login").click();
    h.run_steps(2);
    wait_for_label(h, "1 pending change");
}

#[test]
fn role_edits_are_not_dropped_without_asking() {
    let (mut h, _driver, _dir) = harness(full_admin());
    open_users(&mut h);
    edit_reader(&mut h);

    // Another role: asks; Cancel keeps the edit.
    h.get_by_label("me (you)").click();
    wait_for_label(&mut h, "Discard the pending changes of reader?");
    click_in_modal(&mut h, "Cancel");
    h.run_steps(5);
    h.get_by_label("1 pending change");

    // New role: asks; Discard drops the edit and opens the new role.
    h.get_by_label("New role").click();
    wait_for_label(&mut h, "Discard the pending changes of reader?");
    click_in_modal(&mut h, "Discard");
    step_until(&mut h, "the new role", |h| {
        h.query(By::new().role(Role::PasswordInput)).is_some()
            && h.query_by_label("Discard the pending changes of reader?")
                .is_none()
    });

    // Reload with the new role pending: asks too.
    h.get_by_label("Reload").click();
    wait_for_label(&mut h, "Discard the pending changes of the new role?");
    click_in_modal(&mut h, "Discard");
    wait_for_label(&mut h, "Select a role, or create one with New role");
}

#[test]
fn closing_a_users_tab_with_edits_asks() {
    let (mut h, _driver, _dir) = harness(full_admin());
    open_users(&mut h);
    edit_reader(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::W);
    wait_for_label(&mut h, "Discard the pending changes of reader?");
    click_in_modal(&mut h, "Cancel");
    assert_eq!(
        h.state().tab_titles(),
        ["Query 1", "Users and roles (fake-conn)"]
    );
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::W);
    click_in_modal(&mut h, "Discard");
    step_until(&mut h, "the close", |h| {
        h.state().tab_titles() == ["Query 1"]
    });
}

#[test]
fn object_permission_edits_are_pending_changes() {
    let (mut h, _driver, _dir) = harness(full_admin());
    connect(&mut h);
    h.get_by_label("Expand s").click();
    wait_for_label(&mut h, "Expand tables");
    h.get_by_label("Expand tables").click();
    wait_for_label(&mut h, "orders");
    context_menu(&mut h, "orders", "Open editor");
    wait_for_label(&mut h, "Permissions");
    h.get_by_label("Permissions").click();
    wait_for_label(&mut h, "INSERT on reader");
    h.get_by_label("INSERT on reader").click();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::W);
    wait_for_label(&mut h, "Discard the pending changes of orders?");
    click_in_modal(&mut h, "Discard");
    step_until(&mut h, "the close", |h| {
        h.state().tab_titles() == ["Query 1"]
    });
}

#[test]
fn read_only_sessions_disable_administration_actions() {
    let (mut h, driver, _dir) = harness_with(full_admin(), true);
    // The profiles are read in the background: let them arrive before connecting.
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    open_sessions(&mut h);
    h.get_by_label("7").click();
    h.run_steps(2);
    assert!(h.get_by_label("Terminate").accesskit_node().is_disabled());
    assert!(
        h.get_by_label("Cancel query")
            .accesskit_node()
            .is_disabled()
    );

    context_menu(&mut h, "fake-conn", "Users and roles");
    wait_for_label(&mut h, "reader");
    edit_reader(&mut h);
    assert!(h.get_by_label("Apply").accesskit_node().is_disabled());
    assert!(h.get_by_label("Drop role…").accesskit_node().is_disabled());
    assert!(
        driver
            .executed()
            .iter()
            .all(|s| s.contains("READ ONLY") || s.is_empty())
    );
}

fn open_sessions(h: &mut Harness<'static, SqlrApp>) {
    connect(h);
    context_menu(h, "fake-conn", "Sessions and locks");
    step_until(h, "the sessions tab", |h| {
        h.state().tab_titles() == ["Query 1", "Sessions and locks (fake-conn)"]
    });
    wait_for_label(h, "this connection");
}

#[test]
fn terminate_confirms_twice_for_own_session() {
    let (mut h, driver, _dir) = harness(full_admin());
    open_sessions(&mut h);
    // The lock list shows who waits on whom.
    h.get_by_label("7 waits for 1");

    // Another session: one confirmation.
    select_session(&mut h, "7");
    h.get_by_label("Terminate").click_accesskit();
    wait_for_label(&mut h, "Terminate session 7?");
    click_in_modal(&mut h, "Terminate the session");
    step_until(&mut h, "the terminate", |_| {
        driver.admin_calls().contains(&"terminate 7".to_string())
    });
    // Let the "Done" message appear before selecting another row: it shifts the table,
    // so a click at the old row position would miss.
    wait_for_label(&mut h, "Done");
    h.run_steps(2);

    // The app's own session: asked twice, and "Close" stops it.
    select_session(&mut h, "1");
    h.get_by_label("Terminate").click_accesskit();
    wait_for_label(&mut h, "Terminate session 1?");
    click_in_modal(&mut h, "Terminate the session");
    wait_for_label(&mut h, "Session 1 is this connection. Terminate it anyway?");
    click_in_modal(&mut h, "Close");
    h.run_steps(20);
    assert!(!driver.admin_calls().contains(&"terminate 1".to_string()));

    h.get_by_label("Terminate").click_accesskit();
    click_in_modal(&mut h, "Terminate the session");
    click_in_modal(&mut h, "Terminate it anyway");
    step_until(&mut h, "the own terminate", |_| {
        driver.admin_calls().contains(&"terminate 1".to_string())
    });

    // Cancel asks once for another session.
    select_session(&mut h, "7");
    h.get_by_label("Cancel query").click_accesskit();
    wait_for_label(&mut h, "Cancel the running query of session 7?");
    click_in_modal(&mut h, "Cancel the query");
    step_until(&mut h, "the cancel", |_| {
        driver.admin_calls().contains(&"cancel 7".to_string())
    });
}

#[test]
fn sessions_refresh_only_while_shown() {
    let (mut h, driver, _dir) = harness(full_admin());
    open_sessions(&mut h);
    let polls = |driver: &FakeDriver| {
        driver
            .admin_calls()
            .iter()
            .filter(|c| *c == "sessions")
            .count()
    };
    let first = polls(&driver);
    // Every 2 s while the tab is shown (10 ms per step).
    for _ in 0..450 {
        h.step();
    }
    step_until(&mut h, "two more polls", |_| polls(&driver) >= first + 2);

    // Paused: no more polls.
    h.get_by_label("Pause").click();
    h.run_steps(2);
    let paused = polls(&driver);
    for _ in 0..300 {
        h.step();
    }
    assert_eq!(polls(&driver), paused);
    h.get_by_label("Resume").click();
    h.run_steps(2);

    // Hidden behind another tab: no more polls.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::T);
    h.run_steps(5);
    std::thread::sleep(Duration::from_millis(200));
    h.run_steps(5);
    let hidden = polls(&driver);
    for _ in 0..450 {
        h.step();
    }
    assert_eq!(polls(&driver), hidden, "a hidden tab does not poll");
}

#[test]
fn server_tab_shows_properties_sizes_and_stats() {
    let (mut h, driver, _dir) = harness(full_admin());
    connect(&mut h);
    context_menu(&mut h, "fake-conn", "Server properties");
    step_until(&mut h, "the server tab", |h| {
        h.state().tab_titles() == ["Query 1", "Server properties (fake-conn)"]
    });
    wait_for_label(&mut h, "max_connections");
    h.get_by_label("Memory");
    h.get_by_label("4096 kB");
    h.get_by_label("Database sizes").click();
    wait_for_label(&mut h, "20 MB");
    h.get_by_label("Table statistics").click();
    wait_for_label(&mut h, "orders");
    h.get_by_label("1234");
    assert!(driver.admin_calls().contains(&"stats s".to_string()));
}

#[test]
fn panels_follow_the_drivers_support() {
    // A SQLite-like driver: properties and statistics only.
    let sqlite_like = FakeAdmin {
        support: AdminSupport {
            properties: true,
            statistics: true,
            ..Default::default()
        },
        ..full_admin()
    };
    let (mut h, _driver, _dir) = harness(sqlite_like);
    connect(&mut h);
    h.run_steps(100);
    h.get_by_label("fake-conn").click_secondary();
    h.run_steps(2);
    wait_for_label(&mut h, "Server properties");
    assert!(h.query_by_label("Users and roles").is_none());
    assert!(h.query_by_label("Sessions and locks").is_none());
    h.key_press(egui::Key::Escape);
    h.run_steps(2);

    // ⌘K lists the panels of the active tab's connection.
    let (mut h, _driver, _dir) = harness(full_admin());
    connect(&mut h);
    h.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::P,
    );
    h.run_steps(2);
    h.event(egui::Event::Text("sessions and".into()));
    wait_for_label(&mut h, "Sessions and locks: fake-conn");
    h.key_press(egui::Key::Enter);
    step_until(&mut h, "the sessions tab", |h| {
        h.state().tab_titles() == ["Query 1", "Sessions and locks (fake-conn)"]
    });
}

#[test]
fn object_permissions_and_statistics() {
    let (mut h, driver, _dir) = harness(full_admin());
    connect(&mut h);
    h.get_by_label("Expand s").click();
    wait_for_label(&mut h, "Expand tables");
    h.get_by_label("Expand tables").click();
    wait_for_label(&mut h, "orders");
    context_menu(&mut h, "orders", "Open editor");
    wait_for_label(&mut h, "Permissions");
    // Properties shows the table's statistics.
    h.get_by_label("Properties").click();
    wait_for_label(&mut h, "1234");
    h.get_by_label("16 KB");

    // Permissions: one row per role.
    h.get_by_label("Permissions").click();
    wait_for_label(&mut h, "INSERT on reader");
    h.get_by_label("INSERT on reader").click();
    h.run_steps(2);
    wait_for_label(&mut h, "1 pending change");
    h.get_by_label("Apply grants").click();
    step_until(&mut h, "the grant", |_| {
        driver
            .executed()
            .contains(&"GRANT INSERT ON s.orders TO reader".to_string())
    });
}

#[path = "support/visual.rs"]
mod visual;

#[test]
#[ignore = "visual audit; needs a GPU adapter"]
fn admin_visual_audit() {
    for light in [false, true] {
        let (mut h, _, _dir) = harness(full_admin());
        visual::set_theme(&mut h, light);
        open_users(&mut h);
        h.get_by_label("reader").click();
        h.run_steps(5);
        visual::capture(&mut h, "admin-roles", light);
        h.get_by_label("New role").click();
        h.run_steps(3);
        visual::capture(&mut h, "admin-new-role", light);

        let (mut h, _, _dir) = harness(full_admin());
        visual::set_theme(&mut h, light);
        open_sessions(&mut h);
        visual::capture(&mut h, "admin-sessions-locks", light);

        let (mut h, _, _dir) = harness(full_admin());
        visual::set_theme(&mut h, light);
        connect(&mut h);
        context_menu(&mut h, "fake-conn", "Server properties");
        wait_for_label(&mut h, "max_connections");
        visual::capture(&mut h, "admin-properties", light);
        h.get_by_label("Database sizes").click();
        wait_for_label(&mut h, "20 MB");
        visual::capture(&mut h, "admin-database-sizes", light);
        h.get_by_label("Table statistics").click();
        wait_for_label(&mut h, "1234");
        visual::capture(&mut h, "admin-table-statistics", light);
    }
}
