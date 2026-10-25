//! Diagnostics through the interface: the startup banner and the Diagnostics window.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_app::diagnostics::{Diagnostics, IncidentKind, now_ms, timestamp_prefix};
use sqlr_app::{AppDeps, SqlrApp};
use sqlr_core::{ConnectionConfig, Secrets};
use sqlr_engine::{DriverRegistry, Engine};
use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};
use sqlr_testkit::{FakeDriver, FakeScript};

const TIMEOUT: Duration = Duration::from_secs(5);

fn deps(dir: &std::path::Path, diagnostics: Arc<Diagnostics>) -> AppDeps {
    let mut drivers = DriverRegistry::default();
    drivers.register(FakeDriver::new(FakeScript::default()));
    let secrets = Arc::new(MemorySecretStore::default());
    let engine = Engine::new(drivers, Arc::new(|| {}));
    engine.set_secret_store(secrets.clone());
    let repo = ConnectionRepo::open(dir, Box::new(secrets)).unwrap();
    repo.save(
        &ConnectionConfig {
            id: "c1".into(),
            name: "fake-conn".into(),
            driver: "fake".into(),
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
    AppDeps {
        check_updates: false,
        engine,
        repo,
        settings: Settings::default(),
        config_dir: dir.to_path_buf(),
        data_dir: Some(dir.to_path_buf()),
        startup_notice: None,
        storage_warning: None,
        mcp_token_store: None,
        diagnostics: Some(diagnostics),
        diagnostics_warning: None,
    }
}

fn harness(dir: &tempfile::TempDir) -> (Harness<'static, SqlrApp>, Arc<Diagnostics>) {
    let diagnostics = Arc::new(Diagnostics::open(Some(dir.path())));
    let app = SqlrApp::new(
        deps(dir.path(), diagnostics.clone()),
        &egui::Context::default(),
    );
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    (h, diagnostics)
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
    }
}

fn wait_for_label(h: &mut Harness<'static, SqlrApp>, label: &str) {
    step_until(h, label, |h| h.query_by_label(label).is_some());
}

/// Writes the marker and (with `panic`) a panic incident a previous session would have left.
fn previous_session(dir: &std::path::Path, panic: bool) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let diagnostics = dir.join("diagnostics");
    std::fs::create_dir_all(&diagnostics).unwrap();
    std::fs::write(
        diagnostics.join("session.json"),
        format!(
            r#"{{"session_id":"previous-session","pid":999,"started_ms":1,"version":"{version}"}}"#
        ),
    )
    .unwrap();
    let at = now_ms();
    let id = format!("{}-999-abc", timestamp_prefix(at));
    if panic {
        let incident = serde_json::json!({
            "schema": 1,
            "id": id,
            "session_id": "previous-session",
            "at_ms": at,
            "kind": "panic",
            "message": "assertion failed: the previous run crashed",
            "thread": "main",
            "file": "src/app.rs",
            "line": 42,
            "environment": {"version": version, "os": "macos", "arch": "aarch64"},
            "logs": ["the line before the crash"]
        });
        std::fs::write(
            diagnostics.join(format!("{id}.json")),
            serde_json::to_string_pretty(&incident).unwrap(),
        )
        .unwrap();
    }
    id
}

#[test]
fn a_previous_panic_shows_the_full_banner_and_dismiss_is_remembered() {
    let dir = tempfile::tempdir().unwrap();
    let id = previous_session(dir.path(), true);
    let (mut h, _diagnostics) = harness(&dir);
    wait_for_label(&mut h, "sqlr crashed the last time it ran.");
    h.get_by_label("Open report").click();
    h.run_steps(2);
    assert!(h.state().diagnostics_ui_for_tests().open);
    assert_eq!(
        h.state().diagnostics_ui_for_tests().selected.as_deref(),
        Some(id.as_str())
    );

    h.get_by_label("Dismiss").click();
    h.run_steps(2);
    assert!(h.state().diagnostics_ui_for_tests().notice.is_none());
    assert!(
        h.query_by_label("sqlr crashed the last time it ran.")
            .is_none()
    );
    let diagnostics = h.state().diagnostics_for_tests().unwrap().clone();
    diagnostics.flush();
    assert!(diagnostics.incidents()[0].acknowledged);
    drop(h);
    // A later launch does not announce the dismissed incident.
    let next = Diagnostics::open(Some(dir.path()));
    assert!(next.take_startup_notice().is_none());
    next.clean_exit();
}

#[test]
fn the_crash_banner_opens_a_prefilled_github_issue() {
    let dir = tempfile::tempdir().unwrap();
    previous_session(dir.path(), true);
    let (mut h, _) = harness(&dir);
    wait_for_label(&mut h, "Report on GitHub");
    h.get_by_label("Report on GitHub").click();
    step_until(&mut h, "the crash issue", |h| {
        h.state().diagnostics_ui_for_tests().issue.is_some()
            && h.state().diagnostics_ui_for_tests().preview_open
    });
    let draft = h.state().diagnostics_ui_for_tests().issue.clone().unwrap();
    assert!(draft.title.starts_with("Crash"));
    assert!(draft.body.contains("the previous run crashed"));
}

#[test]
fn an_unclean_shutdown_uses_the_lighter_wording() {
    let dir = tempfile::tempdir().unwrap();
    previous_session(dir.path(), false);
    let (mut h, diagnostics) = harness(&dir);
    wait_for_label(&mut h, "The previous session did not close cleanly.");
    assert!(
        h.query_by_label("sqlr crashed the last time it ran.")
            .is_none()
    );
    assert_eq!(
        diagnostics.incidents()[0].kind,
        IncidentKind::UncleanShutdown
    );
}

#[test]
fn the_diagnostics_window_lists_an_incident_and_toggles_the_sql_setting() {
    let dir = tempfile::tempdir().unwrap();
    let diagnostics = Arc::new(Diagnostics::open(Some(dir.path())));
    diagnostics.record_manual("the migration stopped half way".into(), true, true);
    let app = SqlrApp::new(deps(dir.path(), diagnostics), &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);

    h.get_by_label("Help").click();
    h.run_steps(2);
    h.get_by_label("Diagnostics…").click();
    h.run_steps(3);
    assert!(h.state().diagnostics_ui_for_tests().open);
    step_until(&mut h, "the incident row", |h| {
        h.query_by_label_contains("the migration stopped half way")
            .is_some()
    });
    h.get_by_label_contains("the migration stopped half way")
        .click();
    h.run_steps(2);
    assert!(
        h.query_by_label_contains("the line before the crash")
            .is_none()
    );

    let before = h.state().setting_text("diagnostics.include_sql", None);
    h.get_by_label("Include SQL in logs and reports")
        .scroll_to_me();
    h.run_steps(5);
    h.get_by_label("Include SQL in logs and reports").click();
    h.run_steps(3);
    let after = h.state().setting_text("diagnostics.include_sql", None);
    assert_ne!(before, after, "the setting toggled");
    assert_eq!(after, "false");
}

#[test]
fn the_diagnostics_window_works_without_a_database_or_vault() {
    // No repository rows and no connected engine: the window still draws its summary.
    let dir = tempfile::tempdir().unwrap();
    let diagnostics = Arc::new(Diagnostics::open(Some(dir.path())));
    let app = SqlrApp::new(deps(dir.path(), diagnostics), &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1200.0, 800.0))
        .with_step_dt(0.01)
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    h.get_by_label("Help").click();
    h.run_steps(2);
    h.get_by_label("Diagnostics…").click();
    h.run_steps(3);
    wait_for_label(&mut h, "This session");
}

#[test]
fn the_reviewed_bundle_can_become_a_prefilled_github_issue() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = harness(&dir);
    h.get_by_label("Help").click();
    h.run_steps(2);
    h.get_by_label("Diagnostics…").click();
    h.run_steps(3);
    h.get_by_label("Open GitHub issue…").click();
    step_until(&mut h, "the prefilled issue", |h| {
        h.state().diagnostics_ui_for_tests().issue.is_some()
            && h.state().diagnostics_ui_for_tests().preview_open
    });

    let draft = h.state().diagnostics_ui_for_tests().issue.clone().unwrap();
    assert!(draft.url.starts_with(sqlr_app::diagnostics::NEW_ISSUE));
    assert!(draft.title.starts_with("Bug report"));
    assert!(draft.body.contains("### Environment"));

    h.get_by_label("Open GitHub issue with this report").click();
    h.run_steps(2);
    assert!(!h.state().diagnostics_ui_for_tests().preview_open);
    assert!(
        h.state()
            .diagnostics_ui_for_tests()
            .status
            .as_deref()
            .unwrap_or("")
            .contains("GitHub issue opened")
    );
}

#[test]
fn copying_diagnostics_first_shows_a_preview() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = harness(&dir);
    h.get_by_label("Help").click();
    h.run_steps(2);
    h.get_by_label("Diagnostics…").click();
    h.run_steps(3);
    h.get_by_label("Copy diagnostics").click();
    step_until(&mut h, "bundle preview", |h| {
        h.state().diagnostics_ui_for_tests().preview_open
    });
    assert!(
        h.state()
            .diagnostics_ui_for_tests()
            .preview
            .as_ref()
            .unwrap()
            .contains("session")
    );
    assert!(h.query_by_label("Copy reviewed bundle").is_some());
}

#[test]
fn an_engine_panic_records_one_incident_and_later_queries_work() {
    let dir = tempfile::tempdir().unwrap();
    let diagnostics = Arc::new(Diagnostics::open(Some(dir.path())));
    let _logging =
        sqlr_app::init_logging(None, diagnostics.session_id(), Some(diagnostics.clone()));
    let _hook = sqlr_app::diagnostics::PanicHookGuard::install(diagnostics.clone());
    let mut drivers = DriverRegistry::default();
    drivers.register(FakeDriver::new(FakeScript {
        panic_matching: Some("boom".into()),
        ..Default::default()
    }));
    let engine = Engine::new(drivers, Arc::new(|| {}));
    engine.connect(
        ConnectionConfig {
            id: "c".into(),
            name: "test".into(),
            driver: "fake".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: false,
            agent_access: Default::default(),
        },
        Secrets::default(),
        sqlr_core::ConnectionProfile::development(),
    );
    let deadline = Instant::now() + TIMEOUT;
    while !matches!(
        engine.session_status("c"),
        Some(sqlr_engine::SessionStatus::Connected(_))
    ) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let q = engine.execute("c", "select boom".into(), 10);
    while !matches!(
        engine.result(q).unwrap().read().status,
        sqlr_engine::QueryStatus::Failed(_)
    ) || diagnostics.incidents().is_empty()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    diagnostics.flush();
    assert_eq!(diagnostics.incidents().len(), 1);
    let incident = diagnostics
        .read_incident(&diagnostics.incidents()[0].id)
        .unwrap();
    assert_eq!(incident.kind, IncidentKind::EnginePanic);
    assert_eq!(incident.fields["conn"], "c");
    let q = engine.execute("c", "select 1".into(), 10);
    while !matches!(
        engine.result(q).unwrap().read().status,
        sqlr_engine::QueryStatus::Ready { .. }
    ) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_bundle_excludes_saved_passwords_and_the_mcp_token() {
    let dir = tempfile::tempdir().unwrap();
    let diagnostics = Arc::new(Diagnostics::open(Some(dir.path())));
    let mut dependencies = deps(dir.path(), diagnostics);
    let connection = dependencies.repo.list().unwrap().remove(0);
    dependencies
        .repo
        .save(
            &connection,
            &Secrets(
                [
                    ("password".into(), "password-sentinel-9432".into()),
                    ("token".into(), "connection-token-sentinel-7823".into()),
                ]
                .into(),
            ),
        )
        .unwrap();
    dependencies.mcp_token_store = Some(Arc::new(sqlr_store::MemoryMcpTokenStore::with_token(
        "mcp-token-sentinel-1736",
    )));
    let app = SqlrApp::new(dependencies, &egui::Context::default());
    let mut h = Harness::builder()
        .with_size((1400.0, 900.0))
        .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
    h.run_steps(2);
    h.get_by_label("Help").click();
    h.run_steps(2);
    h.get_by_label("Diagnostics…").click();
    h.run_steps(3);
    h.get_by_label("Copy diagnostics").click();
    step_until(&mut h, "bundle", |h| {
        h.state().diagnostics_ui_for_tests().preview.is_some()
    });
    let text = h
        .state()
        .diagnostics_ui_for_tests()
        .preview
        .as_ref()
        .unwrap();
    serde_json::from_str::<serde_json::Value>(text).unwrap();
    for secret in [
        "password-sentinel-9432",
        "connection-token-sentinel-7823",
        "mcp-token-sentinel-1736",
    ] {
        assert!(!text.contains(secret));
    }
    assert!(
        text.contains("fake-conn"),
        "non-secret connection metadata is included"
    );
}
