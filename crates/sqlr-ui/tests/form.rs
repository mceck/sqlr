use egui::Event;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sqlr_core::{Capabilities, Driver, DriverManifest, Family, FieldSpec, FieldType, Secrets};
use sqlr_testkit::{FakeDriver, FakeScript};
use sqlr_ui::{FormAction, FormState, TestStatus, connection_form};

fn manifest() -> DriverManifest {
    FakeDriver::new(FakeScript::default()).manifest().clone()
}

#[test]
fn defaults_from_manifest() {
    let s = FormState::new(&manifest());
    assert_eq!(s.values["host"], "fake-host");
    assert_eq!(s.values["port"], "1");
    assert_eq!(s.name, "");
    assert!(!s.values.contains_key("password"));
}

#[test]
fn validate_required_and_port() {
    let m = manifest();
    let mut s = FormState::new(&m);
    s.values.insert("port".into(), "70000".into());
    let errors = s.validate(&m);
    assert!(errors.contains(&"Name is required".to_string()));
    assert!(errors.contains(&"Port must be 1–65535".to_string()));
    s.name = "x".into();
    s.values.insert("port".into(), "5432".into());
    s.values.insert("host".into(), " ".into());
    assert_eq!(s.validate(&m), vec!["Host is required".to_string()]);
}

#[test]
fn to_config_splits_secrets() {
    let m = manifest();
    let mut s = FormState::new(&m);
    s.name = "db".into();
    s.group = "prod".into();
    s.secrets.insert("password".into(), "hunter2".into());
    let (cfg, secrets) = s.to_config("id1".into(), &m);
    assert_eq!(cfg.id, "id1");
    assert_eq!(cfg.driver, "fake");
    assert_eq!(cfg.group.as_deref(), Some("prod"));
    assert!(!cfg.values.contains_key("password"));
    assert!(!cfg.values.values().any(|v| v == "hunter2"));
    assert_eq!(secrets.0["password"], "hunter2");
    assert!(!format!("{s:?}").contains("hunter2"));
}

#[test]
fn from_config_round_trips() {
    let m = manifest();
    let mut s = FormState::new(&m);
    s.name = "db".into();
    s.color = Some([1, 2, 3]);
    s.values.insert("host".into(), "h".into());
    s.secrets.insert("password".into(), "pw".into());
    let (cfg, secrets) = s.to_config("id".into(), &m);
    let back = FormState::from_config(&cfg, &secrets, &m);
    assert_eq!(back.name, "db");
    assert_eq!(back.color, Some([1, 2, 3]));
    assert_eq!(back.values["host"], "h");
    assert_eq!(back.secrets["password"], "pw");
    let empty = FormState::from_config(&cfg, &Secrets::default(), &m);
    assert!(empty.secrets.is_empty());
}

struct App {
    state: FormState,
    manifest: DriverManifest,
    test: Option<TestStatus>,
    last: FormAction,
}

fn harness(name: &str) -> Harness<'static, App> {
    let manifest = manifest();
    let mut state = FormState::new(&manifest);
    state.name = name.into();
    let app = App {
        state,
        manifest,
        test: None,
        last: FormAction::None,
    };
    let mut h = Harness::builder()
        .with_size(vec2(500.0, 600.0))
        .build_ui_state(
            |ui, app: &mut App| {
                let a = connection_form(ui, &mut app.state, &app.manifest, app.test.as_ref());
                if a != FormAction::None {
                    app.last = a;
                }
            },
            app,
        );
    h.run_steps(2);
    h
}

#[test]
fn save_disabled_until_valid() {
    let mut h = harness("");
    assert!(h.get_by_label("Save").accesskit_node().is_disabled());
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert_eq!(h.state().last, FormAction::None);
    h.get_by_label("Name is required");

    let mut h = harness("db");
    assert!(!h.get_by_label("Save").accesskit_node().is_disabled());
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert_eq!(h.state().last, FormAction::Save);
}

#[test]
fn test_and_cancel_buttons() {
    let mut h = harness("db");
    h.get_by_label("Test").click();
    h.run_steps(2);
    assert_eq!(h.state().last, FormAction::Test);
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert_eq!(h.state().last, FormAction::Cancel);
}

#[test]
fn test_status_is_shown() {
    let mut h = harness("db");
    h.state_mut().test = Some(TestStatus::Ok("16.1".into()));
    h.run_steps(2);
    h.get_by_label("Connected: 16.1");
    h.state_mut().test = Some(TestStatus::Failed("refused".into()));
    h.run_steps(2);
    h.get_by_label("refused");
}

fn rich_field(
    key: &str,
    label: &str,
    ty: FieldType,
    default: Option<&str>,
    group: &str,
) -> FieldSpec {
    FieldSpec {
        free_form: false,
        key: key.into(),
        label: label.into(),
        secret: ty == FieldType::Password,
        ty,
        default: default.map(String::from),
        required: false,
        group: group.into(),
    }
}

fn rich_manifest() -> DriverManifest {
    DriverManifest {
        id: "rich".into(),
        name: "Rich".into(),
        family: Family::Relational,
        capabilities: Capabilities::default(),
        fields: vec![
            rich_field("host", "Host", FieldType::Text, Some("localhost"), "Main"),
            rich_field("port", "Port", FieldType::Port, Some("5432"), "Main"),
            rich_field("database", "Database", FieldType::Text, None, "Main"),
            rich_field("user", "User", FieldType::Text, None, "Main"),
            rich_field(
                "path",
                "Key file",
                FieldType::File {
                    save: false,
                    extensions: vec!["pem".into()],
                },
                None,
                "Main",
            ),
            rich_field(
                "ssl_mode",
                "SSL mode",
                FieldType::Choice(vec![
                    ("disable".into(), "Disable".into()),
                    ("require".into(), "Require".into()),
                    ("verify_full".into(), "Verify full".into()),
                ]),
                Some("disable"),
                "TLS",
            ),
            rich_field("ssh_host", "SSH host", FieldType::Text, None, "SSH"),
            rich_field(
                "timeout",
                "Timeout",
                FieldType::Number,
                Some("15"),
                "Advanced",
            ),
        ],
        dialect: "rich".into(),
        url_schemes: vec!["postgresql".into(), "postgres".into()],
        default_port: Some(5432),
        editing: None,
    }
}

fn rich_harness() -> Harness<'static, App> {
    let manifest = rich_manifest();
    let mut state = FormState::new(&manifest);
    state.name = "db".into();
    let app = App {
        state,
        manifest,
        test: None,
        last: FormAction::None,
    };
    let mut h = Harness::builder()
        .with_size(vec2(600.0, 700.0))
        .build_ui_state(
            |ui, app: &mut App| {
                let a = connection_form(ui, &mut app.state, &app.manifest, app.test.as_ref());
                if a != FormAction::None {
                    app.last = a;
                }
            },
            app,
        );
    h.run_steps(2);
    h
}

#[test]
fn form_groups_as_tabs() {
    let mut h = rich_harness();
    for tab in ["Main", "TLS", "SSH", "Advanced"] {
        h.get_by_label(tab);
    }
    assert!(h.query_by_label("SSH host").is_none());
    h.get_by_label("SSH").click();
    h.run_steps(2);
    h.get_by_label("SSH host");
    assert!(h.query_by_label("Host").is_none());
    // A single group shows no tabs.
    let h = harness("db");
    assert!(h.query_by_label("Main").is_none());
}

#[test]
fn form_file_field_emits_pick() {
    let mut h = rich_harness();
    h.get_by_label("Browse…").click();
    h.run_steps(2);
    assert_eq!(h.state().last, FormAction::PickFile { key: "path".into() });
}

#[test]
fn form_choice_field() {
    let mut h = rich_harness();
    assert_eq!(h.state().state.values["ssl_mode"], "disable");
    h.get_by_label("TLS").click();
    h.run_steps(2);
    h.get_by_label("SSL mode").click();
    h.run_steps(2);
    h.get_by_label("Require").click();
    h.run_steps(2);
    assert_eq!(h.state().state.values["ssl_mode"], "require");

    let m = rich_manifest();
    let mut s = FormState::new(&m);
    s.name = "x".into();
    assert!(s.validate(&m).is_empty(), "{:?}", s.validate(&m));
    s.values.insert("timeout".into(), "1.5".into());
    s.values.insert("ssl_mode".into(), "bogus".into());
    let errors = s.validate(&m);
    assert!(
        errors.contains(&"Timeout must be a whole number".to_string()),
        "{errors:?}"
    );
    assert!(
        errors.contains(&"SSL mode has an unknown value".to_string()),
        "{errors:?}"
    );
}

fn paste_url(h: &mut Harness<'static, App>, url: &str) {
    h.get_by_label("URL").click();
    h.run_steps(2);
    h.event(Event::Paste(url.into()));
    h.run_steps(2);
}

#[test]
fn form_url_paste_fills_fields() {
    let mut h = rich_harness();
    paste_url(
        &mut h,
        "postgresql://bob@db.example:6543/shop?sslmode=require",
    );
    let values = &h.state().state.values;
    assert_eq!(values["host"], "db.example");
    assert_eq!(values["port"], "6543");
    assert_eq!(values["database"], "shop");
    assert_eq!(values["user"], "bob");
    assert_eq!(values["ssl_mode"], "require");

    let mut h = rich_harness();
    paste_url(&mut h, "postgresql://bob:secret@h/db");
    h.get_by_label("Remove the password from the URL; enter it in the Password field");
    assert_eq!(h.state().state.values["host"], "localhost");
    let dbg = format!("{:?}", h.state().state);
    assert!(!dbg.contains("bob:secret"), "{dbg}");
    assert_eq!(h.state().state.url, "postgresql://bob:***@h/db");

    // A password typed before the `@` is never echoed.
    let mut h = rich_harness();
    paste_url(&mut h, "postgresql://bob:secr");
    let dbg = format!("{:?}", h.state().state);
    assert!(!dbg.contains("secr\"") && !dbg.contains(":secr"), "{dbg}");
    assert!(h.query_by_label_contains("secr").is_none());

    let mut h = rich_harness();
    paste_url(&mut h, "mysql://h/db");
    h.get_by_label("This connection takes postgresql:// or postgres:// URLs");
    // The fake driver declares no URL schemes, so its form has no URL box.
    let h = harness("db");
    assert!(h.query_by_label("URL").is_none());
}

#[test]
fn blank_optional_properties_are_not_saved() {
    let mut manifest = manifest();
    manifest
        .fields
        .extend(sqlr_core::driver_property_fields("sqlite"));
    let mut state = FormState::new(&manifest);
    state.name = "test".into();
    let (cfg, _) = state.to_config("id".into(), &manifest);
    assert!(!cfg.values.keys().any(|k| k.starts_with("property.")));
    state
        .values
        .insert("property.cache_size".into(), "not a number".into());
    assert!(
        state
            .validate(&manifest)
            .iter()
            .any(|s| s.contains("cache_size"))
    );
}

#[test]
fn free_form_properties_validate_and_round_trip() {
    let mut manifest = manifest();
    manifest.fields = sqlr_core::driver_property_fields("postgres");
    let mut state = FormState::new(&manifest);
    state.name = "test".into();
    state
        .values
        .insert("property.work_mem".into(), "8MB".into());
    assert!(state.validate(&manifest).is_empty());
    let (cfg, _) = state.to_config("id".into(), &manifest);
    assert_eq!(
        cfg.values.get("property.work_mem").map(String::as_str),
        Some("8MB")
    );
    let loaded = FormState::from_config(&cfg, &Secrets::default(), &manifest);
    assert_eq!(loaded.values["property.work_mem"], "8MB");
    state.values.insert("property.bad;key".into(), "8MB".into());
    assert!(!state.validate(&manifest).is_empty());
}
