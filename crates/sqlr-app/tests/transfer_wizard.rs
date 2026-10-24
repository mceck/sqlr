//! The data transfer wizard, end to end on two SQLite files.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_core::{ConnectionConfig, Secrets};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};

const TIMEOUT: Duration = Duration::from_secs(20);

fn sqlite_config(id: &str, name: &str, path: &Path) -> ConnectionConfig {
    ConnectionConfig {
        id: id.into(),
        name: name.into(),
        driver: "sqlite".into(),
        group: None,
        color: None,
        values: [("path".to_string(), path.display().to_string())].into(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    }
}

/// An app with two saved SQLite connections: `db-a` (with `src`: three rows) and `db-b` (with
/// an empty `src` of the same shape).
fn harness(dir: &Path) -> Harness<'static, SqlrApp> {
    let a = dir.join("a.db");
    let b = dir.join("b.db");
    rusqlite::Connection::open(&a)
        .unwrap()
        .execute_batch(
            "CREATE TABLE src (id INTEGER PRIMARY KEY, name TEXT, score REAL);
             INSERT INTO src VALUES (1, 'ann', 1.5), (2, 'bob', NULL), (3, 'cy', 3.0);",
        )
        .unwrap();
    rusqlite::Connection::open(&b)
        .unwrap()
        .execute_batch("CREATE TABLE src (id INTEGER, name TEXT, score REAL);")
        .unwrap();
    let mut drivers = DriverRegistry::default();
    drivers.register(SqliteDriver::new());
    let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let engine = Engine::new(drivers, repaint);
    let secrets = Arc::new(MemorySecretStore::default());
    engine.set_secret_store(secrets.clone());
    let repo = ConnectionRepo::open(dir, Box::new(secrets)).unwrap();
    repo.save(&sqlite_config("a", "db-a", &a), &Secrets::default())
        .unwrap();
    repo.save(&sqlite_config("b", "db-b", &b), &Secrets::default())
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
    h
}

fn wait_for_label(h: &mut Harness<'static, SqlrApp>, label: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        h.step();
        if h.query_by_label(label).is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_label_containing(h: &mut Harness<'static, SqlrApp>, text: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        h.step();
        if h.query_by_label_contains(text).is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {text:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn context_menu(h: &mut Harness<'static, SqlrApp>, label: &str, item: &str) {
    h.run_steps(100);
    h.get_all_by_label(label)
        .min_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
        .unwrap_or_else(|| panic!("no node labeled {label}"))
        .click_secondary();
    wait_for_label(h, item);
    h.get_by_label(item).click();
    h.run_steps(2);
}

fn context_menu_sub(h: &mut Harness<'static, SqlrApp>, label: &str, submenu: &str, item: &str) {
    h.run_steps(100);
    h.get_all_by_label(label)
        .min_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
        .unwrap_or_else(|| panic!("no node labeled {label}"))
        .click_secondary();
    let submenu = format!("{submenu} ⏵");
    wait_for_label(h, &submenu);
    h.get_by_label(&submenu).hover();
    h.run_steps(30);
    wait_for_label(h, item);
    h.get_by_label(item).click();
    h.run_steps(2);
}

/// Opens `src` of `db-a` in the explorer and starts "Transfer data…" on it.
fn open_wizard(h: &mut Harness<'static, SqlrApp>) {
    h.get_by_label("Expand db-a").click();
    wait_for_label(h, "Expand main");
    h.get_by_label("Expand main").click();
    wait_for_label(h, "Expand tables");
    h.get_by_label("Expand tables").click();
    wait_for_label(h, "src");
    context_menu_sub(h, "src", "Import/Export", "Transfer data…");
    wait_for_label(h, "Load preview");
}

#[test]
fn transfer_wizard_end_to_end_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    open_wizard(&mut h);

    // Source: the table is prefilled; the preview shows its columns and rows.
    h.get_by_label("Load preview").click();
    wait_for_label(&mut h, "3 columns, first 3 rows");
    h.get_by_label("Next").click();
    h.run_steps(3);

    // Target: an existing table, by default on the other connection and with the same name.
    h.get_by_label("Next").click();
    // Mapping: columns map by name; the table's columns load first.
    wait_for_label(&mut h, "score");
    h.run_steps(3);
    h.get_by_label("Next").click();
    // Options, then the review.
    h.run_steps(3);
    h.get_by_label("Next").click();
    h.run_steps(3);
    wait_for_label_containing(&mut h, "3 of 3 columns mapped");
    h.get_by_label("Next").click();
    h.run_steps(3);
    h.get_by_label("Finish").click();
    wait_for_label(&mut h, "Read 3 rows, wrote 3, rejected 0");

    let copy = rusqlite::Connection::open(dir.path().join("b.db")).unwrap();
    let rows: Vec<(i64, String)> = copy
        .prepare("SELECT id, name FROM src ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        vec![(1, "ann".into()), (2, "bob".into()), (3, "cy".into())]
    );
}

#[test]
fn save_transfer_as_task_and_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    open_wizard(&mut h);
    h.get_by_label("Load preview").click();
    wait_for_label(&mut h, "3 columns, first 3 rows");
    h.get_by_label("Next").click();
    h.run_steps(3);
    h.get_by_label("Next").click();
    wait_for_label(&mut h, "score");
    h.run_steps(3);
    for _ in 0..3 {
        h.get_by_label("Next").click();
        h.run_steps(3);
    }
    h.get_by_label("Finish").click();
    wait_for_label(&mut h, "Read 3 rows, wrote 3, rejected 0");

    // Save it: the prompt is prefilled with a name.
    h.get_by_label("Save as task").click();
    wait_for_label(&mut h, "Save task");
    h.get_by_label("Save task").click();
    h.run_steps(5);
    h.get_by_label("Cancel").click();
    h.run_steps(3);

    // The Tasks pane lists it; running it again appends the rows once more.
    h.get_by_label("Tasks").click();
    wait_for_label(&mut h, "Transfer data");
    h.get_by_label("Transfer data").click();
    h.run_steps(3);
    h.get_by_label("Run task").click();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        h.step();
        let count: i64 = rusqlite::Connection::open(dir.path().join("b.db"))
            .unwrap()
            .query_row("SELECT count(*) FROM src", [], |r| r.get(0))
            .unwrap();
        if count == 6 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the task never ran again: {count} rows"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // The run is on record.
    wait_for_label_containing(&mut h, "wrote 3");

    // The run's summary opens its log.
    h.get_by_label_contains("Read 3 rows, wrote 3, rejected 0")
        .click();
    h.run_steps(5);
    h.get_by_label("Copy").click();
    h.run_steps(2);

    // Duplicate the task, edit the copy, then delete it.
    h.get_by_label("Transfer data").click();
    h.run_steps(2);
    h.get_by_label("Duplicate").click();
    wait_for_label(&mut h, "Transfer data (copy)");
    h.get_all_by_label("Edit")
        .find(|node| node.rect().top() > 60.0)
        .expect("the tasks pane's Edit")
        .click();
    wait_for_label(&mut h, "Load preview");
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    h.get_by_label("Transfer data (copy)").click();
    h.run_steps(2);
    h.get_all_by_label("Delete")
        .find(|node| node.rect().top() > 60.0)
        .expect("the tasks pane's Delete")
        .click();
    let deadline = Instant::now() + TIMEOUT;
    while h.query_by_label("Transfer data (copy)").is_some() {
        assert!(Instant::now() < deadline, "the copied task was not deleted");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn compare_wizard_opens_script_tab() {
    let dir = tempfile::tempdir().unwrap();
    // `db-a` gets a table `db-b` lacks.
    rusqlite::Connection::open(dir.path().join("a.db"))
        .unwrap()
        .execute_batch("CREATE TABLE only_in_a (id INTEGER PRIMARY KEY);")
        .unwrap();
    let mut h = harness(dir.path());
    h.get_by_label("Expand db-a").click();
    wait_for_label(&mut h, "Expand main");
    context_menu(&mut h, "main", "Compare with…");
    wait_for_label(&mut h, "Compare");
    h.get_by_label("Compare").click();
    wait_for_label_containing(&mut h, "1 to create");
    h.get_by_label("Open migration script").click();
    wait_for_label(&mut h, "Migrate main.sql");
}

#[test]
fn er_diagram_from_schema() {
    let dir = tempfile::tempdir().unwrap();
    rusqlite::Connection::open(dir.path().join("a.db"))
        .unwrap()
        .execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY);
             CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id));",
        )
        .unwrap();
    let mut h = harness(dir.path());
    h.get_by_label("Expand db-a").click();
    wait_for_label(&mut h, "Expand main");
    context_menu(&mut h, "main", "ER diagram");
    wait_for_label(&mut h, "ER diagram · main");
    wait_for_label(&mut h, "Export SVG");
}

#[test]
fn generate_data_preview_then_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    // b.db's empty `src` (id, name, score) gets 100 generated rows.
    h.get_by_label("Expand db-b").click();
    wait_for_label(&mut h, "Expand main");
    h.get_by_label("Expand main").click();
    wait_for_label(&mut h, "Expand tables");
    h.get_by_label("Expand tables").click();
    wait_for_label(&mut h, "src");
    context_menu_sub(&mut h, "src", "Import/Export", "Generate data…");
    wait_for_label(&mut h, "Preview");
    h.get_by_label("Preview").click();
    h.run_steps(3);
    h.get_by_label("Generate").click();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        h.step();
        let count: i64 = rusqlite::Connection::open(dir.path().join("b.db"))
            .unwrap()
            .query_row("SELECT count(*) FROM src", [], |r| r.get(0))
            .unwrap();
        if count == 100 {
            break;
        }
        assert!(Instant::now() < deadline, "generated {count} rows");
        std::thread::sleep(Duration::from_millis(20));
    }
    wait_for_label_containing(&mut h, "Wrote 100 rows");
}

#[test]
fn grid_clipboard_transfers_between_databases_with_mapping_and_new_ids() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    let b_path = dir.path().join("b.db");
    rusqlite::Connection::open(&b_path).unwrap().execute_batch(
        "DROP TABLE src; CREATE TABLE src (id INTEGER PRIMARY KEY AUTOINCREMENT, score REAL, name TEXT NOT NULL);
         INSERT INTO src VALUES (100, 0, 'seed'); DELETE FROM src;"
    ).unwrap();
    h.get_by_label("Expand db-a").click();
    wait_for_label(&mut h, "Expand main");
    h.get_by_label("Expand main").click();
    wait_for_label(&mut h, "Expand tables");
    h.get_by_label("Expand tables").click();
    wait_for_label(&mut h, "src");
    context_menu(&mut h, "src", "Open data");
    wait_for_label(&mut h, "ann");
    context_menu(&mut h, "ann", "Copy all results");
    wait_for_label(&mut h, "Copied 3 rows");
    // The native paste event carries text; sqlr keeps its typed counterpart in the context.
    let text = "id\tname\tscore\n1\tann\t1.5\n2\tbob\t\n3\tcy\t3\n";
    h.get_by_label("Collapse db-a").click();
    h.run_steps(2);
    h.get_by_label("Expand db-b").click();
    wait_for_label(&mut h, "Expand main");
    h.get_by_label("Expand main").click();
    wait_for_label(&mut h, "Expand tables");
    h.get_by_label("Expand tables").click();
    wait_for_label(&mut h, "src");
    context_menu(&mut h, "src", "Open data");
    wait_for_label(&mut h, "Paste as new rows…");
    h.get_by_label("Paste as new rows…").click();
    h.run_steps(2);
    h.event(egui::Event::Paste(text.into()));
    wait_for_label(&mut h, "Add pending rows");
    h.get_by_label("Add pending rows").click();
    wait_for_label(&mut h, "3 pending changes");
    let db = rusqlite::Connection::open(&b_path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM src", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    h.get_by_label("Apply").click();
    wait_for_label(&mut h, "Applied 3 changes");
    let actual: Vec<(i64, String, Option<f64>)> = db
        .prepare("SELECT id, name, score FROM src ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        actual,
        vec![
            (101, "ann".into(), Some(1.5)),
            (102, "bob".into(), None),
            (103, "cy".into(), Some(3.0))
        ]
    );
}
