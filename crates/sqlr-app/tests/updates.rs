//! The update prompt (spec §8.3): a newer version is offered with its two actions, a skip
//! is remembered by the next launch, and a dismissal is not.
//!
//! Nothing here reaches the network: the harness runs with `check_updates` off and the
//! prompt is offered directly, the way the daily check would offer it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
use sqlr_testkit::{FakeDriver, FakeScript};

const TIMEOUT: Duration = Duration::from_secs(5);

/// An app on a temp config directory, without the update check.
fn harness(dir: &tempfile::TempDir) -> Harness<'static, SqlrApp> {
    let mut drivers = DriverRegistry::default();
    drivers.register(FakeDriver::new(FakeScript::default()));
    let engine = Engine::new(drivers, Arc::new(|| {}));
    let secrets = Arc::new(MemorySecretStore::default());
    engine.set_secret_store(secrets.clone());
    let repo = ConnectionRepo::open(dir.path(), Box::new(secrets)).unwrap();
    let deps = AppDeps {
        check_updates: false,
        engine,
        repo,
        settings: Settings::default(),
        config_dir: dir.path().to_path_buf(),
        data_dir: Some(dir.path().to_path_buf()),
        startup_notice: None,
        storage_warning: None,
        mcp_token_store: None,
        diagnostics: None,
        diagnostics_warning: None,
    };
    let app = SqlrApp::new(deps, &egui::Context::default());
    Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app)
}

fn step_until(
    h: &mut Harness<'static, SqlrApp>,
    what: &str,
    done: impl Fn(&Harness<'static, SqlrApp>) -> bool,
) {
    let deadline = Instant::now() + TIMEOUT;
    while !done(h) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.run_steps(1);
    }
}

#[test]
fn a_newer_version_is_offered_with_both_actions() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    h.state_mut().offer_update_for_tests("9.9.9");
    step_until(&mut h, "the update prompt", |h| {
        h.state().update_flow_for_tests() == "available"
    });
    h.run_steps(2);
    // The primary action depends on whether this install can replace itself here: a
    // `.app` bundle on macOS, any writable folder on Linux, and never on Windows.
    let primary = h.state().update_primary_label_for_tests();
    assert!(h.query_by_label(primary).is_some(), "no {primary} button");
    assert!(
        h.query_by_label("Skip this version").is_some(),
        "no Skip this version button"
    );
}

#[test]
fn skip_hides_that_version_from_every_launch_after() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    h.state_mut().offer_update_for_tests("9.9.9");
    step_until(&mut h, "the update prompt", |h| {
        h.state().update_flow_for_tests() == "available"
    });
    h.run_steps(2);
    h.get_by_label("Skip this version").click();
    step_until(&mut h, "the update prompt to close", |h| {
        h.state().update_flow_for_tests() == "idle"
    });
    step_until(&mut h, "the skip to be saved", |_| {
        std::fs::read_to_string(dir.path().join("updates.json"))
            .is_ok_and(|text| text.contains("9.9.9"))
    });

    assert_eq!(h.state().skipped_version_for_tests(), "9.9.9");
    let stored = std::fs::read_to_string(dir.path().join("updates.json"))
        .expect("the skip is written next to settings.toml");
    assert!(stored.contains("9.9.9"), "{stored}");

    // A later launch reads it back, so the same release is not proposed again.
    let relaunched = harness(&dir);
    assert_eq!(relaunched.state().skipped_version_for_tests(), "9.9.9");
}

#[test]
fn dismissing_remembers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    h.state_mut().offer_update_for_tests("9.9.9");
    step_until(&mut h, "the update prompt", |h| {
        h.state().update_flow_for_tests() == "available"
    });
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);

    assert_eq!(h.state().update_flow_for_tests(), "idle");
    assert_eq!(h.state().skipped_version_for_tests(), "");
    assert!(
        !dir.path().join("updates.json").exists(),
        "a dismissal remembers nothing"
    );
}

#[test]
fn general_offers_a_manual_check_without_using_the_network() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    h.get_by_label("File").click();
    h.run_steps(2);
    h.get_by_label_contains("Settings…").click();
    h.run_steps(3);
    h.get_by_label("General").click();
    h.run_steps(2);

    h.get_by_label("Check for updates now");
    h.get_by_label("Check for updates at startup");
}

#[test]
fn help_offers_a_manual_check() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    h.run_steps(2);
    h.get_by_label("Help").click();
    h.run_steps(2);
    h.get_by_label("Check for Updates…");
}

#[test]
fn a_manual_check_says_when_the_version_is_current() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    let ctx = h.ctx.clone();
    h.state_mut()
        .apply_check_for_tests(&ctx, Some(env!("CARGO_PKG_VERSION")), None, true);
    h.run_steps(1);

    assert_eq!(h.state().update_flow_for_tests(), "idle");
    let toasts = h.state().toast_texts();
    assert!(
        toasts.iter().any(|text| text.contains("is up to date")),
        "{toasts:?}"
    );
}

#[test]
fn a_manual_check_reports_why_it_failed() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    let ctx = h.ctx.clone();
    h.state_mut()
        .apply_check_for_tests(&ctx, None, Some("offline"), true);
    h.run_steps(1);

    assert_eq!(h.state().update_flow_for_tests(), "idle");
    let toasts = h.state().toast_texts();
    assert!(
        toasts
            .iter()
            .any(|text| text == "The update check failed: offline"),
        "{toasts:?}"
    );
}

#[test]
fn an_automatic_check_that_fails_stays_silent() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(&dir);
    let ctx = h.ctx.clone();
    h.state_mut()
        .apply_check_for_tests(&ctx, None, Some("offline"), false);
    h.run_steps(1);

    assert_eq!(h.state().update_flow_for_tests(), "idle");
    assert!(h.state().toast_texts().is_empty());
    assert!(
        !dir.path().join("updates.json").exists(),
        "a failed check leaves the next launch free to try again"
    );
}

#[test]
fn a_manual_check_offers_a_version_skipped_before() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("updates.json"),
        r#"{"skipped_version":"9.9.9","last_check_at":0}"#,
    )
    .unwrap();
    let mut h = harness(&dir);
    assert_eq!(h.state().skipped_version_for_tests(), "9.9.9");

    let ctx = h.ctx.clone();
    h.state_mut()
        .apply_check_for_tests(&ctx, Some("9.9.9"), None, true);
    step_until(&mut h, "the update prompt", |h| {
        h.state().update_flow_for_tests() == "available"
    });
}
