//! Reproducible visual review on isolated SQLite fixtures. No external database or keychain.
//! SQLR_AUDIT_DIR=/tmp/sqlr-ui-audit/before cargo test -p sqlr-app --lib visual_audit -- --ignored
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::{Harness, kittest::Queryable};
use sqlr_core::{
    ConnectionConfig, ConnectionProfile, Level, Node, NodeKind, NodePath, Secrets, TableRef,
};
use sqlr_engine::{DriverRegistry, Engine, SessionStatus};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
use sqlr_ui::theme::ThemePref;

use crate::app::LeftPane;
use crate::database_tools::{DumpDialog, RestoreDialog};
use crate::transfer_dialogs::{ExportDialog, ImportDialog};
use crate::{AppDeps, SqlrApp};

/// The platform the screenshots show (shortcut notation, menus): the one the audit runs on,
/// where kittest would otherwise report Linux.
const AUDIT_OS: egui::os::OperatingSystem = egui::os::OperatingSystem::from_target_os();

fn fixture(dir: &Path, theme: ThemePref) -> SqlrApp {
    fixture_with(dir, theme, true)
}

fn fixture_with(dir: &Path, theme: ThemePref, connections: bool) -> SqlrApp {
    let file = dir.join("northwind.db");
    rusqlite::Connection::open(&file).unwrap().execute_batch(
        "PRAGMA foreign_keys=ON;
         CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT);
         CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers(id), total REAL NOT NULL, status TEXT DEFAULT 'pending');
         CREATE INDEX orders_customer ON orders(customer_id);
         INSERT INTO customers VALUES (1,'Ada Lovelace','ada@example.com'),(2,'Grace Hopper','grace@example.com');
         INSERT INTO orders VALUES (101,1,249.90,'paid'),(102,2,89.50,'pending');
         CREATE VIEW paid_orders AS SELECT * FROM orders WHERE status='paid';"
    ).unwrap();
    let mut drivers = DriverRegistry::default();
    drivers.register(sqlr_driver_sqlite::SqliteDriver::new());
    let engine = Engine::new(drivers, Arc::new(|| {}));
    let repo = ConnectionRepo::open(dir, Box::new(MemorySecretStore::default())).unwrap();
    let seeded: &[(&str, &str)] = if connections {
        &[
            ("northwind", "Northwind · Development"),
            ("archive", "Northwind · Archive"),
        ]
    } else {
        &[]
    };
    for &(id, name) in seeded {
        let cfg = ConnectionConfig {
            id: id.into(),
            name: name.into(),
            driver: "sqlite".into(),
            group: None,
            color: None,
            values: [("path".into(), file.display().to_string())].into(),
            profile: None,
            save_secrets: false,
            agent_access: Default::default(),
        };
        repo.save(&cfg, &Secrets::default()).unwrap();
        engine.connect(cfg, Secrets::default(), ConnectionProfile::development());
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while connections
        && !matches!(
            engine.session_status("northwind"),
            Some(SessionStatus::Connected(_))
        )
    {
        assert!(Instant::now() < deadline, "SQLite fixture did not connect");
        std::thread::sleep(Duration::from_millis(5));
    }
    SqlrApp::new(
        AppDeps {
            check_updates: false,
            engine,
            repo,
            settings: Settings {
                theme,
                ..Settings::default()
            },
            config_dir: dir.into(),
            data_dir: Some(dir.into()),
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
            diagnostics: None,
            diagnostics_warning: None,
        },
        &egui::Context::default(),
    )
}

fn capture(h: &mut Harness<'static, SqlrApp>, out: &Path, name: &str, theme: &str) {
    h.event(egui::Event::PointerMoved(egui::pos2(1390.0, 890.0)));
    h.run_steps(8);
    h.render()
        .expect("GPU render")
        .save(out.join(format!("{name}-{theme}.png")))
        .unwrap();
}

#[test]
#[ignore = "renders the visual audit; requires a GPU adapter"]
fn visual_audit() {
    let out = std::env::var_os("SQLR_AUDIT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/media/screenshots/audit")
        });
    std::fs::create_dir_all(&out).unwrap();
    for (theme, suffix) in [(ThemePref::Dark, "dark"), (ThemePref::Light, "light")] {
        for case in [
            "welcome",
            "query",
            "ddl",
            "plan",
            "drivers",
            "profiles",
            "connection",
            "commands",
            "objects",
            "new-snippet",
            "delete-connection",
            "cancel-all",
            "save-task",
            "snippets",
            "tasks",
            "history",
            "operations",
            "shortcuts",
            "databases",
            "catalog",
            "export",
            "import",
            "compare",
            "transfer",
            "generate",
            "er",
            "dump-postgres",
            "dump-mysql",
            "dump-sqlite",
            "restore-postgres",
            "restore-mysql",
            "restore-sqlite",
            "preferences",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = fixture(dir.path(), theme);
            let table = TableRef {
                database: None,
                schema: Some("main".into()),
                name: "orders".into(),
            };
            let node = Node {
                path: NodePath::root()
                    .child("main")
                    .child("tables")
                    .child("orders"),
                name: "orders".into(),
                kind: NodeKind::Table,
                has_children: true,
                detail: None,
            };
            match case {
                "query" | "plan" => app.select_rows("northwind", &node, 100),
                "ddl" => app.open_ddl("northwind", &node),
                "drivers" => app.open_driver_manager(),
                "profiles" => app.open_profiles(),
                "connection" => {
                    let manifest = app.manifest("sqlite").expect("SQLite manifest");
                    app.conn_dialog = Some(crate::dialogs::ConnDialog::create(&manifest));
                }
                "commands" => app.open_palette(sqlr_ui::PaletteMode::Commands),
                "objects" => app.open_palette(sqlr_ui::PaletteMode::Objects),
                "new-snippet" => app.run_command(
                    crate::palette::AppCommand::SaveSnippet,
                    &egui::Context::default(),
                ),
                "delete-connection" => app.delete_pending = Some("northwind".into()),
                "cancel-all" => app.cancel_all_operations = true,
                "save-task" => app.save_task_dialog(
                    sqlr_core::TaskSpec::Dump {
                        conn: "northwind".into(),
                        database: "northwind".into(),
                        options: Default::default(),
                        out: dir.path().join("backup.sql"),
                    },
                    None,
                ),

                "snippets" => app.left = LeftPane::Snippets,
                "tasks" => app.left = LeftPane::Tasks,
                "history" => app.show_history = true,
                "operations" => app.show_operations = true,
                "shortcuts" => app.shortcuts.open = true,
                "databases" => {
                    app.databases_dialog = Some(crate::databases_dialog::DatabasesDialog::matrix())
                }
                "catalog" => {
                    app.databases_dialog = Some(crate::databases_dialog::DatabasesDialog::catalog())
                }
                "export" => {
                    app.export_dialog = Some(ExportDialog::new("orders", None, None, false))
                }
                "import" => {
                    app.import_dialog = Some(ImportDialog::new("northwind", table, true, false))
                }
                "compare" => app.open_compare(Some("northwind"), None, Some("main".into())),
                "transfer" => app.open_transfer_wizard(Some(("northwind", &node))),
                "generate" => app.open_datagen("northwind", &node),
                "er" => app.open_er_diagram("northwind", None, "main".into(), None),
                "preferences" => {
                    app.prefs = Some(crate::prefs::PrefsWindow::new(&app.layered, Level::Global))
                }
                _ if case.starts_with("dump-") => {
                    app.dump_dialog = DumpDialog::new(
                        "northwind",
                        "Northwind",
                        "northwind",
                        case.trim_start_matches("dump-"),
                    )
                }
                _ if case.starts_with("restore-") => {
                    app.restore_dialog = RestoreDialog::new(
                        "northwind",
                        "Northwind",
                        "northwind",
                        case.trim_start_matches("restore-"),
                    )
                }
                _ => {}
            }
            let mut h = Harness::builder()
                .with_size((1400.0, 900.0))
                .with_os(AUDIT_OS)
                .with_step_dt(0.01)
                .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
            sqlr_ui::theme::install_fonts(&h.ctx, &[]);
            // Allow asynchronous metadata jobs to finish, without depending on server timing.
            for _ in 0..20 {
                h.step();
                std::thread::sleep(Duration::from_millis(5));
            }
            if case == "plan" {
                h.state_mut().explain_active(false);
                for _ in 0..20 {
                    h.step();
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            capture(&mut h, &out, case, suffix);
            if case == "transfer" {
                h.get_by_label("Load preview").click();
                let deadline = Instant::now() + Duration::from_secs(5);
                while h
                    .query_by_label_contains("4 columns, first 2 rows")
                    .is_none()
                {
                    assert!(Instant::now() < deadline, "transfer preview timed out");
                    h.step();
                    std::thread::sleep(Duration::from_millis(5));
                }
                capture(&mut h, &out, "transfer-source-preview", suffix);
                for step in ["target", "mapping", "options", "review", "run"] {
                    h.get_by_label("Next").click();
                    for _ in 0..15 {
                        h.step();
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    capture(&mut h, &out, &format!("transfer-{step}"), suffix);
                }
            }
            if case == "import" {
                let file = dir.path().join("orders.csv");
                std::fs::write(
                    &file,
                    "id,customer_id,total,status\n201,1,54.90,paid\n202,2,123.45,pending\n",
                )
                .unwrap();
                h.state_mut().file_answer = Some(file);
                h.get_by_label("Choose file…").click();
                let deadline = Instant::now() + Duration::from_secs(5);
                while h.query_by_label("First 2 rows").is_none() {
                    assert!(Instant::now() < deadline, "import preview timed out");
                    h.step();
                    std::thread::sleep(Duration::from_millis(5));
                }
                capture(&mut h, &out, "import-preview", suffix);
            }
            if case == "preferences" {
                for section in [
                    "General",
                    "Appearance",
                    "Editor",
                    "Result grid",
                    "Data formats",
                    "Keymap",
                    "Connections",
                    "Agents",
                    "Drivers",
                ] {
                    h.get_all_by_label(section)
                        .max_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
                        .expect("preference section")
                        .click();
                    capture(
                        &mut h,
                        &out,
                        &format!("preferences-{}", section.to_lowercase().replace(' ', "-")),
                        suffix,
                    );
                }
            }
        }
    }
}

#[test]
fn configuration_dialogs_fit_a_small_display() {
    for title in [
        "Preferences",
        "Data Sources and Drivers",
        "Connection profiles",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixture(dir.path(), ThemePref::Dark);
        match title {
            "Preferences" => {
                app.prefs = Some(crate::prefs::PrefsWindow::new(&app.layered, Level::Global))
            }
            "Data Sources and Drivers" => {
                let manifest = app.manifest("sqlite").unwrap();
                app.conn_dialog = Some(crate::dialogs::ConnDialog::create(&manifest));
            }
            _ => app.open_profiles(),
        }
        let mut h = Harness::builder()
            .with_size((800.0, 600.0))
            .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
        h.run_steps(8);
        let label = format!("Close {title}");
        let close = h.get_by_label(&label).rect();
        assert!(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))
                .contains_rect(close),
            "{title} close action outside display: {close:?}"
        );
    }
}

fn settle(h: &mut Harness<'static, SqlrApp>, rounds: usize) {
    for _ in 0..rounds {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn leftmost<'h>(
    h: &'h Harness<'static, SqlrApp>,
    label: &'h str,
) -> Option<egui_kittest::Node<'h>> {
    h.query_all_by_label(label)
        .min_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
}

fn harness(app: SqlrApp, size: (f32, f32)) -> Harness<'static, SqlrApp> {
    let h = Harness::builder()
        .with_size(size)
        .with_os(AUDIT_OS)
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    sqlr_ui::theme::install_fonts(&h.ctx, &[]);
    h
}

/// Stateful screens: results, errors, context menus, empty and failed connections, and the
/// main screens on a small display.
#[test]
#[ignore = "renders the visual audit states; requires a GPU adapter"]
fn visual_audit_states() {
    let out = std::env::var_os("SQLR_AUDIT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/media/screenshots/audit")
        });
    std::fs::create_dir_all(&out).unwrap();
    for (theme, suffix) in [(ThemePref::Dark, "dark"), (ThemePref::Light, "light")] {
        // Empty app: no connections at all.
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(fixture_with(dir.path(), theme, false), (1400.0, 900.0));
        settle(&mut h, 20);
        capture(&mut h, &out, "state-empty-no-connections", suffix);

        // Failed connection.
        let dir = tempfile::tempdir().unwrap();
        let bad = ConnectionConfig {
            id: "broken".into(),
            name: "Broken · Unreachable".into(),
            driver: "sqlite".into(),
            group: None,
            color: None,
            values: [("path".into(), "/nonexistent-dir/x/y.db".into())].into(),
            profile: None,
            save_secrets: false,
            agent_access: Default::default(),
        };
        ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default()))
            .unwrap()
            .save(&bad, &Secrets::default())
            .unwrap();
        let mut app = fixture(dir.path(), theme);
        app.reload_connections();
        app.engine
            .connect(bad, Secrets::default(), ConnectionProfile::development());
        let mut h = harness(app, (1400.0, 900.0));
        settle(&mut h, 40);
        capture(&mut h, &out, "state-connection-failed", suffix);

        // Query error, then a result with the cell menu and the history.
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixture(dir.path(), theme);
        let key = app.new_tab(None, Some("northwind".into()), "SELEC * FROM nowhere;");
        app.focus_tab(key);
        app.run_tab(key);
        let mut h = harness(app, (1400.0, 900.0));
        settle(&mut h, 40);
        capture(&mut h, &out, "state-query-error", suffix);

        let dir = tempfile::tempdir().unwrap();
        let mut app = fixture(dir.path(), theme);
        let key = app.new_tab(
            None,
            Some("northwind".into()),
            "SELECT o.id, c.name, o.total, o.status FROM orders o JOIN customers c ON c.id = o.customer_id;",
        );
        app.focus_tab(key);
        app.run_tab(key);
        let mut h = harness(app, (1400.0, 900.0));
        settle(&mut h, 40);
        capture(&mut h, &out, "state-query-result", suffix);
        if let Some(cell) = leftmost(&h, "Ada Lovelace") {
            cell.click();
            settle(&mut h, 5);
            capture(&mut h, &out, "state-grid-cell-selected", suffix);
            leftmost(&h, "Ada Lovelace").unwrap().click_secondary();
            settle(&mut h, 5);
            capture(&mut h, &out, "state-grid-cell-menu", suffix);
        }

        // Explorer context menus.
        let dir = tempfile::tempdir().unwrap();
        let app = fixture(dir.path(), theme);
        let mut h = harness(app, (1400.0, 900.0));
        settle(&mut h, 20);
        if let Some(n) = leftmost(&h, "Northwind · Development") {
            n.click_secondary();
            settle(&mut h, 5);
            capture(&mut h, &out, "state-menu-connection", suffix);
        }
        let dir = tempfile::tempdir().unwrap();
        let app = fixture(dir.path(), theme);
        let mut h = harness(app, (1400.0, 900.0));
        settle(&mut h, 20);
        if let Some(n) = leftmost(&h, "Northwind · Development") {
            n.click();
            settle(&mut h, 30);
            capture(&mut h, &out, "state-explorer-expanded", suffix);
            for label in ["main", "Tables"] {
                if let Some(n) = leftmost(&h, label) {
                    n.click();
                    settle(&mut h, 20);
                }
            }
            if let Some(n) = leftmost(&h, "orders") {
                n.click_secondary();
                settle(&mut h, 5);
                capture(&mut h, &out, "state-menu-table", suffix);
            }
        }
    }

    // Main screens on a small display (dark only, as agreed).
    for case in [
        "welcome",
        "query",
        "connection",
        "preferences",
        "objects",
        "history",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixture(dir.path(), ThemePref::Dark);
        match case {
            "query" => {
                let key = app.new_tab(None, Some("northwind".into()), "SELECT * FROM orders;");
                app.focus_tab(key);
                app.run_tab(key);
            }
            "connection" => {
                let manifest = app.manifest("sqlite").expect("SQLite manifest");
                app.conn_dialog = Some(crate::dialogs::ConnDialog::create(&manifest));
            }
            "preferences" => {
                app.prefs = Some(crate::prefs::PrefsWindow::new(&app.layered, Level::Global))
            }
            "objects" => app.open_palette(sqlr_ui::PaletteMode::Objects),
            "history" => app.show_history = true,
            _ => {}
        }
        let mut h = harness(app, (1024.0, 680.0));
        settle(&mut h, 40);
        capture(&mut h, &out, &format!("small-{case}"), "dark");
    }
}
