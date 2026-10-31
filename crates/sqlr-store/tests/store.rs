use std::sync::Arc;

use sqlr_core::{
    Capabilities, CommitMode, ConnectionConfig, ConnectionProfile, DriverManifest, Family,
    FieldSpec, FieldType, Secrets, ThemePref,
};
use sqlr_store::{
    ConnectionRepo, KeyringStore, MemorySecretStore, MoveTarget, ProfileRepo, SecretStore,
    Settings, StoreError,
};

fn manifest() -> DriverManifest {
    let field = |key: &str, secret: bool| FieldSpec {
        free_form: false,
        key: key.into(),
        label: key.into(),
        ty: FieldType::Text,
        default: None,
        required: false,
        secret,
        group: "Main".into(),
    };
    DriverManifest {
        id: "postgres".into(),
        name: "Postgres".into(),
        family: Family::Relational,
        capabilities: Capabilities::default(),
        fields: vec![field("host", false), field("password", true)],
        dialect: "postgres".into(),
        url_schemes: Vec::new(),
        default_port: None,
        editing: None,
    }
}

fn cfg(id: &str, name: &str, group: Option<&str>) -> ConnectionConfig {
    ConnectionConfig {
        id: id.into(),
        name: name.into(),
        driver: "postgres".into(),
        group: group.map(Into::into),
        color: None,
        values: [("host".to_string(), "localhost".to_string())].into(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    }
}

fn secrets(pw: &str) -> Secrets {
    Secrets([("password".to_string(), pw.to_string())].into())
}

fn repo(dir: &std::path::Path) -> ConnectionRepo {
    ConnectionRepo::open(dir, Box::new(MemorySecretStore::default())).unwrap()
}

#[test]
fn save_list_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let mut b = cfg("2", "Beta", Some("prod"));
    b.color = Some([1, 2, 3]);
    repo.save(&b, &secrets("hunter2")).unwrap();
    let a = cfg("1", "Alpha", None);
    repo.save(&a, &secrets("")).unwrap();

    assert_eq!(repo.list().unwrap(), vec![b.clone(), a]);
    let text = std::fs::read_to_string(dir.path().join("connections.toml")).unwrap();
    assert!(!text.contains("hunter2"), "{text}");
    assert!(text.contains("[[connection]]"), "{text}");
    assert!(
        text.find("folder =")
            .is_none_or(|i| i < text.find("[[connection]]").unwrap()),
        "the folder list must stay at the top level: {text}"
    );
    assert_eq!(
        repo.secrets_for(&b, &manifest()).unwrap().0["password"],
        "hunter2"
    );
    assert_eq!(repo.folders().unwrap(), vec!["prod"]);
}

#[test]
fn save_upserts_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    repo.save(&cfg("1", "Old", None), &secrets("")).unwrap();
    repo.save(&cfg("1", "New", None), &secrets("")).unwrap();
    let list = repo.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "New");
}

#[test]
fn empty_secret_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let c = cfg("1", "A", None);
    repo.save(&c, &secrets("pw")).unwrap();
    repo.save(&c, &secrets("")).unwrap();
    assert!(
        !repo
            .secrets_for(&c, &manifest())
            .unwrap()
            .0
            .contains_key("password")
    );
}

#[test]
fn delete_removes_config_and_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let c = cfg("1", "A", None);
    repo.save(&c, &secrets("pw")).unwrap();
    repo.delete("1", &manifest()).unwrap();
    assert!(repo.list().unwrap().is_empty());
    assert!(repo.secrets_for(&c, &manifest()).unwrap().0.is_empty());
}

#[test]
fn missing_file_lists_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(repo(dir.path()).list().unwrap().is_empty());
}

#[test]
fn folders_keep_empty_entries_and_implicit_groups() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    repo.save(&cfg("1", "A", Some("prod")), &secrets(""))
        .unwrap();
    repo.create_folder("Empty").unwrap();
    assert_eq!(repo.folders().unwrap(), vec!["prod", "Empty"]);
    assert_eq!(repo.list().unwrap()[0].group.as_deref(), Some("prod"));
}

#[test]
fn create_rename_delete_folder_updates_its_connections() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    repo.create_folder("Prod").unwrap();
    repo.save(&cfg("1", "A", Some("Prod")), &secrets(""))
        .unwrap();
    repo.save(&cfg("2", "B", Some("Prod")), &secrets(""))
        .unwrap();

    repo.rename_folder("Prod", "Production").unwrap();
    assert_eq!(repo.folders().unwrap(), vec!["Production"]);
    assert!(
        repo.list()
            .unwrap()
            .iter()
            .all(|c| c.group.as_deref() == Some("Production"))
    );

    repo.delete_folder("Production").unwrap();
    assert!(repo.folders().unwrap().is_empty());
    assert!(repo.list().unwrap().iter().all(|c| c.group.is_none()));
    assert_eq!(repo.list().unwrap().len(), 2, "connections are kept");
}

#[test]
fn folder_names_are_unique_and_non_empty() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    repo.create_folder("Prod").unwrap();
    assert!(matches!(
        repo.create_folder("Prod"),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        repo.create_folder("   "),
        Err(StoreError::Invalid(_))
    ));
    repo.create_folder("Test").unwrap();
    assert!(matches!(
        repo.rename_folder("Test", "Prod"),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn move_connection_into_folder_and_between_positions() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    repo.create_folder("Prod").unwrap();
    repo.save(&cfg("1", "A", None), &secrets("")).unwrap();
    repo.save(&cfg("2", "B", Some("Prod")), &secrets(""))
        .unwrap();
    repo.save(&cfg("3", "C", Some("Prod")), &secrets(""))
        .unwrap();

    repo.move_connection("1", Some("Prod"), MoveTarget::Before("3".into()))
        .unwrap();
    let groups: Vec<(String, Option<String>)> = repo
        .list()
        .unwrap()
        .into_iter()
        .map(|c| (c.name, c.group))
        .collect();
    assert_eq!(
        groups,
        vec![
            ("B".into(), Some("Prod".into())),
            ("A".into(), Some("Prod".into())),
            ("C".into(), Some("Prod".into())),
        ]
    );

    repo.move_connection("3", None, MoveTarget::End).unwrap();
    let list = repo.list().unwrap();
    assert_eq!(list.last().unwrap().name, "C");
    assert_eq!(list.last().unwrap().group, None);
}

#[test]
fn move_connection_to_unknown_folder_fails() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    repo.save(&cfg("1", "A", None), &secrets("")).unwrap();
    assert!(matches!(
        repo.move_connection("1", Some("nope"), MoveTarget::End),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(repo.list().unwrap()[0].group, None);
}

#[test]
fn nested_folders_list_their_ancestors() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    repo.create_folder("Prod/Staging/EU").unwrap();
    assert_eq!(
        repo.folders().unwrap(),
        vec!["Prod", "Prod/Staging", "Prod/Staging/EU"]
    );

    // A group may name a nested folder the file does not list yet.
    repo.save(&cfg("1", "A", Some("Apps / Reporting")), &secrets(""))
        .unwrap();
    assert_eq!(
        repo.folders().unwrap(),
        vec![
            "Prod",
            "Prod/Staging",
            "Prod/Staging/EU",
            "Apps",
            "Apps/Reporting"
        ]
    );
    assert_eq!(
        repo.list().unwrap()[0].group.as_deref(),
        Some("Apps/Reporting")
    );
}

#[test]
fn renaming_a_folder_renames_its_subtree_and_connections() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    repo.create_folder("Prod/Team").unwrap();
    repo.save(&cfg("1", "A", Some("Prod/Team")), &secrets(""))
        .unwrap();

    repo.rename_folder("Prod", "Production").unwrap();
    assert_eq!(
        repo.folders().unwrap(),
        vec!["Production", "Production/Team"]
    );
    assert_eq!(
        repo.list().unwrap()[0].group.as_deref(),
        Some("Production/Team")
    );

    // A leaf rename keeps the parent; moving a folder is a separate operation.
    repo.rename_folder("Production/Team", "Production/Squad")
        .unwrap();
    assert_eq!(
        repo.list().unwrap()[0].group.as_deref(),
        Some("Production/Squad")
    );
    assert!(matches!(
        repo.rename_folder("Production/Squad", "Elsewhere"),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn deleting_a_folder_promotes_its_subfolders() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    repo.create_folder("Prod/Team/Core").unwrap();
    repo.save(&cfg("1", "A", Some("Prod")), &secrets(""))
        .unwrap();
    repo.save(&cfg("2", "B", Some("Prod/Team")), &secrets(""))
        .unwrap();
    repo.save(&cfg("3", "C", Some("Prod/Team/Core")), &secrets(""))
        .unwrap();

    repo.delete_folder("Prod").unwrap();
    assert_eq!(repo.folders().unwrap(), vec!["Team", "Team/Core"]);
    let groups: Vec<Option<String>> = repo.list().unwrap().into_iter().map(|c| c.group).collect();
    assert_eq!(
        groups,
        vec![None, Some("Team".into()), Some("Team/Core".into())]
    );
}

#[test]
fn moving_a_folder_into_another_moves_its_subtree() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    repo.create_folder("A").unwrap();
    repo.create_folder("B/C").unwrap();
    repo.save(&cfg("1", "X", Some("B/C")), &secrets(""))
        .unwrap();

    let to = repo
        .move_folder("B", &MoveTarget::Into("A".into()))
        .unwrap();
    assert_eq!(to, "A/B");
    assert_eq!(repo.folders().unwrap(), vec!["A", "A/B", "A/B/C"]);
    assert_eq!(repo.list().unwrap()[0].group.as_deref(), Some("A/B/C"));

    let to = repo
        .move_folder("A/B", &MoveTarget::Before("A".into()))
        .unwrap();
    assert_eq!(to, "B");
    assert_eq!(repo.folders().unwrap(), vec!["B", "B/C", "A"]);
    assert_eq!(repo.list().unwrap()[0].group.as_deref(), Some("B/C"));

    assert!(matches!(
        repo.move_folder("A", &MoveTarget::Into("A".into())),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn move_folder_reorders() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    for name in ["A", "B", "C"] {
        repo.create_folder(name).unwrap();
    }
    repo.move_folder("C", &MoveTarget::Before("A".into()))
        .unwrap();
    assert_eq!(repo.folders().unwrap(), vec!["C", "A", "B"]);
    repo.move_folder("C", &MoveTarget::End).unwrap();
    assert_eq!(repo.folders().unwrap(), vec!["A", "B", "C"]);
}

#[test]
fn legacy_file_stays_sorted_until_the_first_write() {
    let dir = tempfile::tempdir().unwrap();
    // A file from before folders: no version, no folder list.
    let text = r#"
[[connection]]
id = "2"
name = "Beta"
driver = "postgres"
group = "prod"

[[connection]]
id = "1"
name = "Alpha"
driver = "postgres"
"#;
    std::fs::write(dir.path().join("connections.toml"), text).unwrap();
    let repo = repo(dir.path());
    let names: Vec<String> = repo.list().unwrap().into_iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["Alpha", "Beta"]);
    assert_eq!(repo.folders().unwrap(), vec!["prod"]);

    // The first write records an explicit order.
    repo.create_folder("Empty").unwrap();
    let names: Vec<String> = repo.list().unwrap().into_iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["Alpha", "Beta"]);
}

struct DenyingStore;

impl SecretStore for DenyingStore {
    fn get(&self, _: &str, _: &str) -> Result<Option<String>, StoreError> {
        Ok(None)
    }
    fn set(&self, _: &str, _: &str, _: &str) -> Result<(), StoreError> {
        Err(StoreError::Secret("denied".into()))
    }
    fn delete(&self, _: &str, _: &str) -> Result<(), StoreError> {
        Ok(())
    }
}

struct FailingDeleteStore(MemorySecretStore);

impl SecretStore for FailingDeleteStore {
    fn get(&self, conn_id: &str, field: &str) -> Result<Option<String>, StoreError> {
        self.0.get(conn_id, field)
    }
    fn set(&self, conn_id: &str, field: &str, value: &str) -> Result<(), StoreError> {
        self.0.set(conn_id, field, value)
    }
    fn delete(&self, _: &str, _: &str) -> Result<(), StoreError> {
        Err(StoreError::Secret("denied".into()))
    }
}

#[test]
fn delete_with_failing_secret_store_keeps_config() {
    let dir = tempfile::tempdir().unwrap();
    let repo = ConnectionRepo::open(
        dir.path(),
        Box::new(FailingDeleteStore(MemorySecretStore::default())),
    )
    .unwrap();
    let c = cfg("1", "A", None);
    repo.save(&c, &secrets("pw")).unwrap();
    let err = repo.delete("1", &manifest()).unwrap_err();
    assert!(matches!(err, StoreError::Secret(_)));
    assert_eq!(repo.list().unwrap(), vec![c]);
}

#[test]
fn save_with_failing_secret_store_keeps_config() {
    let dir = tempfile::tempdir().unwrap();
    let repo = ConnectionRepo::open(dir.path(), Box::new(DenyingStore)).unwrap();
    let c = cfg("1", "A", None);
    let err = repo.save(&c, &secrets("pw")).unwrap_err();
    assert!(matches!(err, StoreError::Secret(_)));
    assert_eq!(repo.list().unwrap(), vec![c]);
}

#[test]
fn settings_default_and_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(Settings::load(dir.path()).unwrap().page_size, 200);
    Settings {
        page_size: 1000,
        ..Settings::default()
    }
    .save(dir.path())
    .unwrap();
    assert_eq!(Settings::load(dir.path()).unwrap().page_size, 1000);
}

#[test]
fn settings_theme_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let saved = Settings {
        theme: ThemePref::Light,
        mono_font_size: 16.0,
        ..Settings::default()
    };
    saved.save(dir.path()).unwrap();
    assert_eq!(Settings::load(dir.path()).unwrap(), saved);

    // A settings.toml from before the appearance settings existed.
    std::fs::write(
        dir.path().join("settings.toml"),
        "page_size = 200\nstop_script_on_error = false\n",
    )
    .unwrap();
    let old = Settings::load(dir.path()).unwrap();
    assert_eq!(old.page_size, 200);
    assert_eq!(old.theme, ThemePref::System);
    assert_eq!(old.mono_font_size, 13.0);

    for (written, loaded) in [("99", 24.0), ("2", 10.0), ("nan", 13.0), ("14.5", 14.5)] {
        std::fs::write(
            dir.path().join("settings.toml"),
            format!("theme = \"Dark\"\nmono_font_size = {written}\n"),
        )
        .unwrap();
        let s = Settings::load(dir.path()).unwrap();
        assert_eq!(s.theme, ThemePref::Dark);
        assert_eq!(s.mono_font_size, loaded, "mono_font_size = {written}");
    }
}

#[test]
fn profiles_builtins_and_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let repo = ProfileRepo::open(dir.path());
    let ids = |repo: &ProfileRepo| -> Vec<String> {
        repo.list().unwrap().into_iter().map(|p| p.id).collect()
    };
    assert_eq!(ids(&repo), ["development", "test", "production"]);
    let test = |repo: &ProfileRepo| repo.list().unwrap().remove(1);
    assert_eq!(test(&repo).commit_mode, CommitMode::Smart);

    // An override of a built-in replaces it in place.
    let mut over = test(&repo);
    over.commit_mode = CommitMode::Auto;
    repo.save(&over).unwrap();
    assert_eq!(test(&repo).commit_mode, CommitMode::Auto);
    assert_eq!(ids(&repo), ["development", "test", "production"]);
    // Deleting it resets the built-in.
    repo.delete("test").unwrap();
    assert_eq!(test(&repo).commit_mode, CommitMode::Smart);

    // A user profile round-trips and is listed after the built-ins, by name.
    let mine = |id: &str, name: &str| ConnectionProfile {
        id: id.into(),
        name: name.into(),
        color: Some([1, 2, 3]),
        commit_mode: CommitMode::Manual,
        confirm_writes: true,
        confirm_destructive: false,
        read_only: true,
        idle_tx_timeout_min: Some(3),
    };
    repo.save(&mine("u2", "Staging")).unwrap();
    repo.save(&mine("u1", "Analytics")).unwrap();
    let list = ProfileRepo::open(dir.path()).list().unwrap();
    assert_eq!(list[3], mine("u1", "Analytics"));
    assert_eq!(list[4], mine("u2", "Staging"));
    assert!(dir.path().join("profiles.toml").exists());

    // Deleting a built-in id without an override is a no-op.
    repo.delete("production").unwrap();
    assert_eq!(ids(&repo).len(), 5);
    repo.delete("u1").unwrap();
    assert_eq!(ids(&repo), ["development", "test", "production", "u2"]);
}

#[test]
fn settings_session_and_timeout_defaults() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("settings.toml"), "page_size = 200\n").unwrap();
    let s = Settings::load(dir.path()).unwrap();
    assert!(s.session_per_tab);
    assert_eq!(s.connect_timeout_s, 15);
    let saved = Settings {
        session_per_tab: false,
        connect_timeout_s: 30,
        ..Settings::default()
    };
    saved.save(dir.path()).unwrap();
    assert_eq!(Settings::load(dir.path()).unwrap(), saved);
}

#[test]
fn unsaved_secrets_stay_out_of_the_keychain() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let mut c = cfg("1", "A", None);
    repo.save(&c, &secrets("hunter2")).unwrap();
    c.save_secrets = false;
    repo.save(&c, &secrets("other")).unwrap();
    assert!(repo.secrets_for(&c, &manifest()).unwrap().0.is_empty());
    let text = std::fs::read_to_string(dir.path().join("connections.toml")).unwrap();
    assert!(
        !text.contains("hunter2") && !text.contains("other"),
        "{text}"
    );
    assert_eq!(repo.list().unwrap(), vec![c]);
}

#[test]
#[ignore = "touches the real OS keychain"]
fn keyring_round_trip() {
    let store: Arc<dyn SecretStore> = Arc::new(KeyringStore);
    let field = "password";
    store.delete("sqlr-test", field).unwrap();
    assert_eq!(store.get("sqlr-test", field).unwrap(), None);
    store.set("sqlr-test", field, "s3cret").unwrap();
    let got = store.get("sqlr-test", field);
    store.delete("sqlr-test", field).unwrap();
    assert_eq!(got.unwrap().as_deref(), Some("s3cret"));
    assert_eq!(store.get("sqlr-test", field).unwrap(), None);
}
