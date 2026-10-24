//! The welcome screen: shown without tabs, the demo database, the
//! recent connections, and the known-databases catalog of the new-connection flow.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_core::{ConnectionConfig, Secrets};
use sqlr_driver_postgres::PostgresDriver;
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
use sqlr_testkit::{FakeDriver, FakeScript};
use sqlr_ui::theme::ThemePref;

const TIMEOUT: Duration = Duration::from_secs(20);

/// An app on `dir` (config) and `dir/data` (data), with the fake, SQLite, MySQL and PostgreSQL
/// drivers and one saved fake connection "fake-conn".
fn harness(dir: &Path) -> Harness<'static, SqlrApp> {
    harness_themed(dir, ThemePref::System)
}

fn harness_themed(dir: &Path, theme: ThemePref) -> Harness<'static, SqlrApp> {
    let mut drivers = DriverRegistry::default();
    drivers.register(FakeDriver::new(FakeScript {
        rows: 3,
        ..Default::default()
    }));
    drivers.register(SqliteDriver::new());
    drivers.register(sqlr_driver_mysql::MysqlDriver::new());
    drivers.register(PostgresDriver::new());
    let engine = Engine::new(drivers, Arc::new(|| {}));
    // The data directory exists in the app (`sqlr.db` lives there); a file in its place
    // (one test) stays a file.
    let _ = std::fs::create_dir_all(dir.join("data"));
    let secrets = Arc::new(MemorySecretStore::default());
    engine.set_secret_store(secrets.clone());
    let repo = ConnectionRepo::open(dir, Box::new(secrets)).unwrap();
    let cfg = ConnectionConfig {
        id: "c1".into(),
        name: "fake-conn".into(),
        driver: "fake".into(),
        group: None,
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    };
    repo.save(&cfg, &Secrets::default()).unwrap();
    let deps = AppDeps {
        engine,
        repo,
        settings: Settings {
            theme,
            ..Settings::default()
        },
        config_dir: dir.to_path_buf(),
        data_dir: Some(dir.join("data")),
        startup_notice: None,
        storage_warning: None,
        mcp_token_store: None,
        diagnostics: None,
        diagnostics_warning: None,
    };
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    h
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
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; toasts: {:?}",
            h.state().toast_texts()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Closes every tab with ⌘W, which brings the welcome screen up.
fn close_all_tabs(h: &mut Harness<'static, SqlrApp>) {
    while !h.state().tab_titles().is_empty() {
        h.key_press_modifiers(Modifiers::COMMAND, Key::W);
        h.run_steps(2);
    }
    h.run_steps(2);
}

#[test]
fn welcome_shown_without_tabs() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    assert!(
        h.state().tab_titles().is_empty(),
        "startup must show the homepage"
    );
    h.get_by_label("Open demo database");
    h.get_by_label("A fast, native database client");
    h.get_by_label("No recent connections yet");
    // A new tab hides it again.
    h.get_by_label("New connection…");
    h.key_press_modifiers(Modifiers::COMMAND, Key::T);
    h.run_steps(2);
    assert!(h.query_by_label("Open demo database").is_none());
    close_all_tabs(&mut h);
    h.get_by_label("Open demo database");
}

#[test]
fn welcome_import_link_opens_connection_import() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    close_all_tabs(&mut h);
    h.get_by_label("Import connections…").click();
    h.run_steps(3);
    h.get_by_label("Import connections");
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    h.get_by_label("Import connections…");
}

/// Rows of `sql`'s single count, read straight from the file.
fn count(path: &Path, table: &str) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn demo_database_opens_and_queries() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    close_all_tabs(&mut h);
    h.get_by_label("Open demo database").click();
    step_until(&mut h, "the demo query's rows", |h| {
        h.query_by_label_contains("100 rows").is_some()
    });
    let demo = h
        .state()
        .saved_connections()
        .into_iter()
        .find(|(_, name, _)| name == "Demo")
        .expect("a saved Demo connection");
    assert_eq!(demo.2, "sqlite");
    assert_eq!(h.state().tab_conns(), [Some(demo.0.clone())]);
    let text = h.state_mut().active_text().unwrap();
    assert_eq!(text, "SELECT * FROM order_summary LIMIT 100;");

    let file = dir.path().join("data").join("demo.sqlite");
    assert_eq!(count(&file, "departments"), 6);
    assert_eq!(count(&file, "employees"), 40);
    assert_eq!(count(&file, "customers"), 200);
    assert_eq!(count(&file, "products"), 60);
    assert_eq!(count(&file, "orders"), 2000);
    let items = count(&file, "order_items");
    assert!((5000..=7000).contains(&items), "{items} items");
    assert_eq!(count(&file, "sqlr_demo_meta"), 1);

    // Opening it again reuses the connection and the file.
    close_all_tabs(&mut h);
    h.get_by_label("Open demo database").click();
    step_until(&mut h, "the second demo tab", |h| {
        h.state().tab_titles().len() == 1 && h.query_by_label_contains("100 rows").is_some()
    });
    let demos = h
        .state()
        .saved_connections()
        .into_iter()
        .filter(|(_, name, _)| name == "Demo")
        .count();
    assert_eq!(demos, 1);
    assert_eq!(count(&file, "orders"), 2000);
}

#[test]
fn demo_never_deletes_foreign_file() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let foreign = data.join("demo.sqlite");
    {
        let conn = rusqlite::Connection::open(&foreign).unwrap();
        conn.execute_batch("CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('mine');")
            .unwrap();
    }
    let mut h = harness(dir.path());
    close_all_tabs(&mut h);
    h.get_by_label("Open demo database").click();
    step_until(&mut h, "the demo query's rows", |h| {
        h.query_by_label_contains("100 rows").is_some()
    });
    // The user's file is untouched; the demo lives next to it.
    assert_eq!(count(&foreign, "notes"), 1);
    let conn = rusqlite::Connection::open(&foreign).unwrap();
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 1);
    assert_eq!(count(&data.join("demo-sqlr.sqlite"), "orders"), 2000);
}

#[test]
fn demo_reports_an_unwritable_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    // A file where the data directory should be: nothing can be created there.
    std::fs::write(dir.path().join("data"), "not a directory").unwrap();
    let mut h = harness(dir.path());
    close_all_tabs(&mut h);
    h.get_by_label("Open demo database").click();
    step_until(&mut h, "the error toast", |h| {
        h.state()
            .toast_texts()
            .iter()
            .any(|t| t.contains("Could not open the demo database"))
    });
    assert!(h.state().tab_titles().is_empty());
}

#[test]
fn recent_connection_opens_an_editor_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    h.get_by_label("Expand fake-conn").click();
    step_until(&mut h, "the recent entry", |h| {
        h.state().recent_ids().first().is_some_and(|id| id == "c1")
    });
    close_all_tabs(&mut h);
    // The welcome's row is the rightmost "fake-conn" (the explorer's is on the left).
    h.get_all_by_label("fake-conn")
        .max_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
        .unwrap()
        .click();
    h.run_steps(3);
    assert_eq!(h.state().tab_conns(), [Some("c1".to_string())]);
}

#[test]
fn compatible_preset_fills_form() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    close_all_tabs(&mut h);
    h.get_by_label("More databases…").click();
    // The modal sizes itself over a few frames.
    h.run_steps(30);
    // Planned databases are listed, but picking one opens nothing.
    h.get_by_label("SQL Server").click();
    h.run_steps(3);
    assert!(h.state().conn_dialog_form().is_none());
    h.get_by_label("CockroachDB").click();
    h.run_steps(3);
    assert!(
        h.query_by_label("SQL Server").is_none(),
        "the catalog closed"
    );
    let (driver, values, name) = h.state().conn_dialog_form().expect("the connection dialog");
    assert_eq!(driver, "postgres");
    assert_eq!(values["port"], "26257");
    assert_eq!(values["database"], "defaultdb");
    assert_eq!(name, "CockroachDB");
}

#[test]
fn welcome_card_opens_the_dialog_on_its_driver() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    close_all_tabs(&mut h);
    h.get_by_label("SQLite").click();
    h.run_steps(3);
    let (driver, _, _) = h.state().conn_dialog_form().expect("the connection dialog");
    assert_eq!(driver, "sqlite");
}

#[test]
fn settings_open_the_databases_matrix() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    h.state_mut().open_databases_matrix_for_tests();
    h.run_steps(30);
    assert!(
        h.query_all_by_label("Needs driver file").count() > 1,
        "the JDBC databases install from a local file"
    );
    assert!(h.query_all_by_label("Planned (later)").count() >= 1);
    // This build has no Redis, MongoDB or Elasticsearch driver: their entries say so.
    assert!(h.query_all_by_label("Driver missing").count() > 1);
    h.get_by_label("Close").click();
    h.run_steps(3);
    assert!(h.query_all_by_label("Needs driver file").next().is_none());
}

#[test]
fn the_connection_dialog_opens_the_catalog() {
    // In the connection dialog, "More databases…" opens the catalog, and a pick switches the
    // dialog to a new data source preset for it.
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    h.key_press_modifiers(Modifiers::COMMAND, Key::T);
    h.run_steps(2);
    // The explorer header's button.
    h.get_by_label("New connection").click();
    h.run_steps(5);
    h.get_by_label("More databases…").click();
    h.run_steps(30);
    h.get_by_label("YugabyteDB").click();
    h.run_steps(3);
    let (driver, values, name) = h.state().conn_dialog_form().expect("the connection dialog");
    assert_eq!(driver, "postgres");
    assert_eq!(values["port"], "5433");
    assert_eq!(name, "YugabyteDB");
}

/// Renders `docs/media/screenshots/welcome-{dark,light}.png`:
/// `cargo test -p sqlr-app --test welcome welcome_screenshots -- --ignored`.
#[test]
#[ignore = "writes screenshots; needs a wgpu adapter"]
fn welcome_screenshots() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/media/screenshots");
    std::fs::create_dir_all(&out).unwrap();
    for (name, pref) in [("dark", ThemePref::Dark), ("light", ThemePref::Light)] {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness_themed(dir.path(), pref);
        sqlr_ui::theme::install_fonts(&h.ctx, &[]);
        h.get_by_label("Expand fake-conn").click();
        step_until(&mut h, "the recent entry", |h| {
            !h.state().recent_ids().is_empty()
        });
        close_all_tabs(&mut h);
        h.run_steps(60);
        let image = h.render().expect("render");
        image.save(out.join(format!("welcome-{name}.png"))).unwrap();
    }
}
