//! The preferences window: search, per-connection overrides, live
//! preview with Cancel, and the data formats reaching the grids.

use std::path::Path;
use std::sync::Arc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_core::{ConnectionConfig, Secrets};
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
use sqlr_testkit::{FakeDriver, FakeScript};
use sqlr_ui::theme::icons;

fn app(dir: &Path) -> Harness<'static, SqlrApp> {
    let mut drivers = DriverRegistry::default();
    drivers.register(FakeDriver::new(FakeScript {
        rows: 3,
        ..Default::default()
    }));
    let engine = Engine::new(drivers, Arc::new(|| {}));
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
    };
    repo.save(&cfg, &Secrets::default()).unwrap();
    let deps = AppDeps {
        engine,
        repo,
        settings: Settings::load(dir).unwrap(),
        config_dir: dir.to_path_buf(),
        data_dir: None,
        startup_notice: None,
        storage_warning: None,
    };
    let app = SqlrApp::new(deps, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(3);
    h
}

fn open_preferences(h: &mut Harness<'static, SqlrApp>) {
    h.get_by_label("File").click();
    h.run_steps(2);
    h.get_by_label_contains("Settings…").click();
    h.run_steps(3);
}

const STOP_ON_ERROR: &str = "Stop a script at its first error";

#[test]
fn prefs_search_filters_sections() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = app(dir.path());
    open_preferences(&mut h);
    h.get_by_label("Theme");
    assert!(h.query_by_label(STOP_ON_ERROR).is_none(), "another section");

    // The search field has the keyboard: a keyword finds a setting in another section.
    h.event(egui::Event::Text("zoom".into()));
    h.run_steps(3);
    h.get_by_label("UI scale");
    assert!(h.query_by_label(STOP_ON_ERROR).is_none());
    assert!(h.query_by_label("Theme").is_none());

    h.event(egui::Event::Text("qqqq".into()));
    h.run_steps(3);
    h.get_by_label("No setting matches the search.");
}

#[test]
fn prefs_override_per_connection_shows_dot() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = app(dir.path());
    assert_eq!(
        h.state()
            .setting_text("general.stop_script_on_error", Some("c1")),
        "true"
    );
    h.state_mut().open_preferences_for_connection("c1");
    h.run_steps(3);
    // Only settings that can differ per connection are listed, with no dot yet.
    h.get_by_label(STOP_ON_ERROR);
    assert!(h.query_by_label("Theme").is_none() && h.query_by_label("Line numbers").is_none());
    assert!(h.query_by_label(icons::DOT).is_none());

    h.get_by_label(STOP_ON_ERROR).click();
    h.run_steps(3);
    h.get_by_label(icons::DOT);
    assert_eq!(
        h.state()
            .setting_text("general.stop_script_on_error", Some("c1")),
        "false"
    );
    // Another resolution, global, is untouched.
    assert_eq!(
        h.state().setting_text("general.stop_script_on_error", None),
        "true"
    );

    // Reset clears the override and its dot.
    h.get_by_label("Reset").click();
    h.run_steps(3);
    assert!(h.query_by_label(icons::DOT).is_none());
    assert_eq!(
        h.state()
            .setting_text("general.stop_script_on_error", Some("c1")),
        "true"
    );

    // Save writes the overrides next to the global file.
    h.get_by_label(STOP_ON_ERROR).click();
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(3);
    let overrides = std::fs::read_to_string(dir.path().join("overrides.toml")).unwrap();
    assert!(overrides.contains("[connection.c1]"), "{overrides}");
}

#[test]
fn prefs_cancel_reverts_live_preview() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = app(dir.path());
    assert!(
        h.ctx.global_style().visuals.dark_mode,
        "System falls back to dark"
    );
    open_preferences(&mut h);
    h.get_by_label("Light").click();
    h.run_steps(3);
    assert!(
        !h.ctx.global_style().visuals.dark_mode,
        "the preview shows at once"
    );
    assert!(
        !dir.path().join("settings.toml").exists(),
        "nothing is written yet"
    );

    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(
        h.ctx.global_style().visuals.dark_mode,
        "Cancel puts the theme back"
    );
    assert!(h.query_by_label("Apply").is_none(), "the window closed");
    assert!(!dir.path().join("settings.toml").exists());
}

#[test]
fn grid_uses_data_formats() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = app(dir.path());
    assert!(
        sqlr_ui::data_formats(&h.ctx).is_none(),
        "defaults leave the grid alone"
    );
    open_preferences(&mut h);
    // Searching brings the setting to the top of the page.
    h.event(egui::Event::Text("booleans".into()));
    h.run_steps(3);
    h.get_by_label("✓ / ✗").click();
    h.run_steps(3);
    assert!(
        sqlr_ui::data_formats(&h.ctx).is_some(),
        "the grids follow the preferences"
    );
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(
        sqlr_ui::data_formats(&h.ctx).is_none(),
        "and Cancel puts them back"
    );
}

#[test]
fn corrupt_settings_show_a_toast_and_keep_the_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("settings.toml"), "page_size = [broken\n").unwrap();
    let h = app(dir.path());
    let toasts = h.state().toast_texts();
    assert!(
        toasts.iter().any(|t| t.contains("settings.toml.corrupt")),
        "{toasts:?}"
    );
    assert!(dir.path().join("settings.toml.corrupt").exists());
}

#[test]
fn saving_preferences_waits_for_background_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = app(dir.path());
    open_preferences(&mut h);
    h.get_by_label("Save").click();
    h.run_steps(1);
    h.get_by_label("Saving preferences…");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while h.query_by_label("Apply").is_some() {
        assert!(std::time::Instant::now() < deadline);
        h.run_steps(1);
        std::thread::yield_now();
    }
    assert!(dir.path().join("settings.toml").exists());
    assert!(dir.path().join("keymap.toml").exists());
}
