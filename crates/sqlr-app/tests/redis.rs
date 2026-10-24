//! The Redis tab in the app, against the fake key-value driver.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_core::{ConnectionConfig, KvNodePath, KvPage, Node, NodeKind, Secrets};
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
use sqlr_testkit::{FakeKvDriver, FakeKvScript};

const TIMEOUT: Duration = Duration::from_secs(5);

fn script() -> FakeKvScript {
    let db0 = KvNodePath::database(0);
    // The explorer names differ from the key browser's (`usr`, `one`), so a label finds one.
    let usr = KvNodePath::group(&db0, b"user:");
    let node = |path, name: &str, kind, has_children, detail: Option<&str>| Node {
        path,
        name: name.into(),
        kind,
        has_children,
        detail: detail.map(String::from),
    };
    FakeKvScript {
        keys: vec![
            (
                b"user:1".to_vec(),
                KvPage::String {
                    bytes: b"hello".to_vec(),
                    total_len: 5,
                },
            ),
            (
                b"session".to_vec(),
                KvPage::Hash {
                    fields: vec![(b"id".to_vec(), b"42".to_vec())],
                    cursor: 0,
                },
            ),
        ],
        tree: vec![
            node(db0.clone(), "db0", NodeKind::Database, true, Some("2 keys")),
            node(
                KvNodePath::database(3),
                "db3",
                NodeKind::Database,
                false,
                Some("1 key"),
            ),
            node(usr.clone(), "usr", NodeKind::Folder, true, Some("1 key")),
            node(
                KvNodePath::key(&usr, b"user:1"),
                "one",
                NodeKind::Table,
                false,
                Some("string"),
            ),
        ],
        ..Default::default()
    }
}

fn app(
    dir: &std::path::Path,
    profile: Option<&str>,
) -> (Harness<'static, SqlrApp>, Arc<FakeKvDriver>) {
    let driver = FakeKvDriver::new(script());
    let mut drivers = DriverRegistry::default();
    drivers.register(driver.clone());
    let engine = Engine::new(drivers, Arc::new(|| {}));
    let repo = ConnectionRepo::open(dir, Box::new(MemorySecretStore::default())).unwrap();
    repo.save(
        &ConnectionConfig {
            id: "r1".into(),
            name: "redis-conn".into(),
            driver: "fakekv".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: profile.map(String::from),
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

fn console_input(h: &mut Harness<'static, SqlrApp>) {
    wait_for_label(h, "Console");
    h.get_by_label("Console").click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Console input")
        .focus();
    h.run_steps(2);
}

fn run_line(h: &mut Harness<'static, SqlrApp>, line: &str) {
    h.event(egui::Event::Text(line.into()));
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(2);
}

#[test]
fn redis_tab_opens_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _driver) = app(dir.path(), None);
    h.get_by_label("Expand redis-conn").click();
    // Connecting opens the browser on the configured database.
    step_until(&mut h, "the browser", |h| {
        h.state()
            .tab_titles()
            .iter()
            .any(|t| t == "redis-conn (db0)")
    });
    wait_for_label(&mut h, "Expand db0");
    h.get_by_label("Expand db0").click();
    wait_for_label(&mut h, "Expand usr");
    h.get_by_label("Expand usr").click();
    wait_for_label(&mut h, "one");
    double_click(&mut h, "one");
    // The key opens in the browser that is already there: its value shows.
    step_until(&mut h, "the value", |h| {
        h.query_by_role_and_label(Role::TextInput, "String value")
            .is_some()
    });
    let titles = h.state().tab_titles();
    assert_eq!(
        titles.iter().filter(|t| *t == "redis-conn (db0)").count(),
        1,
        "{titles:?}"
    );
}

#[test]
fn redis_browser_lists_keys_and_loads_a_value() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _driver) = app(dir.path(), None);
    h.get_by_label("Expand redis-conn").click();
    wait_for_label(&mut h, "Expand user");
    // `session` is a key without a separator: a leaf of the browser.
    wait_for_label(&mut h, "session");
    h.get_by_label("session").click();
    step_until(&mut h, "the hash", |h| h.query_by_label("id").is_some());
    h.get_by_label("42");
    h.get_by_label("Edit id");
}

#[test]
fn redis_console_runs_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = app(dir.path(), None);
    h.get_by_label("Expand redis-conn").click();
    console_input(&mut h);
    run_line(&mut h, "PING");
    step_until(&mut h, "the reply", |h| h.query_by_label("PONG").is_some());
    h.get_by_label("> PING");
    assert_eq!(driver.commands().len(), 1);
}

#[test]
fn production_confirms_a_console_write() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = app(dir.path(), Some("production"));
    h.get_by_label("Expand redis-conn").click();
    console_input(&mut h);
    run_line(&mut h, "SET k v");
    wait_for_label(
        &mut h,
        "Run 1 statement that modifies data on redis-conn db0?",
    );
    assert!(
        driver.commands().is_empty(),
        "nothing runs before the answer"
    );
    // The dialog's "Run" comes after the toolbar's.
    h.get_all_by_label("Run").last().unwrap().click();
    step_until(&mut h, "the command to run", |_| {
        driver.commands().len() == 1
    });
}

#[test]
fn production_confirms_flushdb_as_destructive() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = app(dir.path(), Some("production"));
    h.get_by_label("Expand redis-conn").click();
    console_input(&mut h);
    run_line(&mut h, "FLUSHDB");
    wait_for_label(
        &mut h,
        "This will FLUSHDB on redis-conn db0 (production profile).",
    );
    h.get_by_label("Cancel").click();
    h.run_steps(5);
    assert!(h.query_by_label_contains("This will").is_none());
    assert!(driver.commands().is_empty(), "{:?}", driver.commands());
}

#[test]
fn production_asks_before_a_console_line_shows_no_secret() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _driver) = app(dir.path(), Some("production"));
    h.get_by_label("Expand redis-conn").click();
    console_input(&mut h);
    run_line(&mut h, "AUTH hunter2");
    wait_for_label(
        &mut h,
        "Run 1 statement that modifies data on redis-conn db0?",
    );
    // The confirmation lists the line without its password.
    h.get_by_label("AUTH ***");
    assert!(h.query_by_label_contains("hunter2").is_none());
}

#[test]
fn production_confirms_a_value_edit_and_a_key_delete() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = app(dir.path(), Some("production"));
    h.get_by_label("Expand redis-conn").click();
    wait_for_label(&mut h, "session");
    h.get_by_label("session").click();
    step_until(&mut h, "the hash", |h| {
        h.query_by_label("Remove id").is_some()
    });
    h.get_by_label("Remove id").click();
    wait_for_label(
        &mut h,
        "Run 1 statement that modifies data on redis-conn db0?",
    );
    h.get_by_label("HDEL session id");
    h.get_by_label("Cancel").click();
    h.run_steps(5);
    assert!(driver.edits().is_empty());
    // A key delete from the value view is destructive.
    h.get_by_label("Delete key").click();
    h.run_steps(3);
    h.get_all_by_label("Delete").last().unwrap().click();
    // The widget's own dialog asked about deleting; the profile asks as for any write.
    wait_for_label(
        &mut h,
        "Run 1 statement that modifies data on redis-conn db0?",
    );
    h.get_by_label("DEL session");
    h.get_all_by_label("Run").last().unwrap().click();
    step_until(&mut h, "the delete", |_| driver.edits().len() == 1);
    assert_eq!(driver.key(0, b"session"), None);
}

#[test]
fn a_console_runs_on_its_own_tabs_database_and_the_prompt_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = app(dir.path(), Some("production"));
    h.get_by_label("Expand redis-conn").click();
    wait_for_label(&mut h, "db3");
    h.run_steps(100);
    h.get_by_label("db3").click_secondary();
    wait_for_label(&mut h, "Open key browser");
    h.get_by_label("Open key browser").click();
    step_until(&mut h, "the db3 browser", |h| {
        h.state()
            .tab_titles()
            .iter()
            .any(|t| t == "redis-conn (db3)")
    });
    console_input(&mut h);
    run_line(&mut h, "SET k v");
    wait_for_label(
        &mut h,
        "Run 1 statement that modifies data on redis-conn db3?",
    );
    h.get_all_by_label("Run").last().unwrap().click();
    step_until(&mut h, "the command", |_| driver.commands().len() == 1);
    assert_eq!(driver.commands()[0].0, 3);
}

#[test]
fn cmd_n_on_a_redis_tab_explains_instead_of_opening_an_unbound_tab() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _driver) = app(dir.path(), None);
    h.get_by_label("Expand redis-conn").click();
    step_until(&mut h, "the browser", |h| {
        h.state()
            .tab_titles()
            .iter()
            .any(|t| t == "redis-conn (db0)")
    });
    let before = h.state().tab_titles();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run_steps(3);
    assert_eq!(h.state().tab_titles(), before, "no unbound tab");
    let toasts = h.state().toast_texts();
    assert!(
        toasts
            .iter()
            .any(|t| t.contains("redis-conn") && t.contains("explorer")),
        "{toasts:?}"
    );
}

#[test]
fn read_only_switch_on_a_redis_tab_refuses_a_console_write() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, driver) = app(dir.path(), None);
    h.get_by_label("Expand redis-conn").click();
    console_input(&mut h);
    // A key-value tab has no transaction: only the Read-only switch shows.
    for label in [
        "Tx",
        "Commit",
        "Rollback",
        "Savepoint",
        "Rollback to savepoint",
    ] {
        assert!(h.query_by_label(label).is_none(), "{label} shows");
    }
    h.get_by_label("Read-only").click();
    step_until(&mut h, "read-only to reach the driver", |_| {
        driver.read_only()
    });
    console_input(&mut h);
    run_line(&mut h, "SET k v");
    step_until(&mut h, "the refusal", |h| {
        h.query_by_label_contains("read-only").is_some()
    });
    assert!(driver.commands().is_empty(), "{:?}", driver.commands());
}

use crate::visual;

#[test]
#[ignore = "visual audit; needs a GPU adapter"]
fn redis_visual_audit() {
    for light in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (mut h, _) = app(dir.path(), None);
        visual::set_theme(&mut h, light);
        h.get_by_label("Expand redis-conn").click();
        wait_for_label(&mut h, "session");
        visual::capture(&mut h, "redis-browser", light);
        h.get_by_label("New key").click();
        h.run_steps(5);
        visual::capture(&mut h, "redis-new-key", light);
        h.get_by_label("Cancel").click();
        h.run_steps(5);
        h.get_by_label("session").click();
        wait_for_label(&mut h, "Edit id");
        visual::capture(&mut h, "redis-hash", light);
        h.get_by_label("Delete key").click();
        h.run_steps(5);
        visual::capture(&mut h, "redis-delete-key", light);
        h.get_by_label("Cancel").click();
        h.run_steps(5);
        console_input(&mut h);
        run_line(&mut h, "PING");
        wait_for_label(&mut h, "PONG");
        visual::capture(&mut h, "redis-console", light);
    }
}
