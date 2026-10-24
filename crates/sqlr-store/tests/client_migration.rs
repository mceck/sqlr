use sqlr_core::{AgentAccess, ConnectionConfig};
use sqlr_store::client_migration::{ClientFormat, decode, encode};

fn pg() -> ConnectionConfig {
    ConnectionConfig {
        id: "original".into(),
        name: "R&D <production>".into(),
        driver: "postgres".into(),
        group: Some("Team/Production".into()),
        color: Some([12, 34, 56]),
        values: [
            ("host", "db.example.com"),
            ("port", "5433"),
            ("database", "sales"),
            ("user", "alice"),
            ("ssl_mode", "verify-full"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect(),
        profile: None,
        save_secrets: true,
        agent_access: AgentAccess::Write,
    }
}

#[test]
fn imports_real_dbeaver_shape_and_ignores_credentials() {
    let batch = decode(
        ClientFormat::Dbeaver,
        br#"{
      "connections": {
        "pg-1": {"provider":"postgresql", "driver":"postgres-jdbc", "name":"Production",
          "folder":"Team/Production", "configuration":{"host":"db.internal","port":"5433",
          "database":"app","user":"alice","password":"NEVER_STORE_THIS",
          "properties":{"sslmode":"verify-full"}}},
        "other": {"provider":"unknown", "driver":"unknown", "name":"Unsupported"}
      }}"#,
    )
    .unwrap();
    assert_eq!(batch.connections.len(), 1);
    let c = &batch.connections[0];
    assert_eq!(c.driver, "postgres");
    assert_eq!(c.values["host"], "db.internal");
    assert_eq!(c.values["ssl_mode"], "verify-full");
    assert_eq!(c.group.as_deref(), Some("Team/Production"));
    assert_eq!(c.agent_access, AgentAccess::None);
    assert!(!c.save_secrets);
    assert!(!format!("{batch:?}").contains("NEVER_STORE_THIS"));
    assert!(!batch.warnings.is_empty());
}

#[test]
fn imports_datagrip_clipboard_xml_with_escaping_and_jdbc_parameters() {
    let batch = decode(
        ClientFormat::Datagrip,
        br#"<data-sources>
      <data-source name="R&amp;D" uuid="external"><driver-ref>postgresql</driver-ref>
        <jdbc-url>jdbc:postgresql://db:5432/sales?sslmode=verify-full</jdbc-url>
        <user-name>alice</user-name></data-source></data-sources>"#,
    )
    .unwrap();
    assert_eq!(batch.connections[0].name, "R&D");
    assert_eq!(batch.connections[0].values["database"], "sales");
    assert_eq!(batch.connections[0].values["user"], "alice");
    assert_eq!(batch.connections[0].values["ssl_mode"], "verify-full");
}

#[test]
fn imports_tableplus_plist_without_reading_keychain() {
    let batch = decode(
        ClientFormat::TablePlus,
        br#"<?xml version="1.0"?><plist version="1.0"><array><dict>
      <key>ID</key><string>external</string><key>ConnectionName</key><string>Local</string>
      <key>Driver</key><string>MySQL</string><key>DatabaseHost</key><string>localhost</string>
      <key>DatabasePort</key><integer>3307</integer><key>DatabaseUser</key><string>root</string>
      <key>DatabaseName</key><string>app</string></dict></array></plist>"#,
    )
    .unwrap();
    assert_eq!(batch.connections[0].driver, "mysql");
    assert_eq!(batch.connections[0].values["port"], "3307");
}

#[test]
fn supported_exports_round_trip_and_never_copy_agent_consent_or_passwords() {
    for format in ClientFormat::ALL {
        let mut c = pg();
        c.values
            .insert("password".into(), "NEVER_EXPORT_THIS".into());
        c.values
            .insert("ssh_password".into(), "NEVER_EXPORT_THIS".into());
        let doc = encode(format, &[c]).unwrap();
        assert!(!doc.contents.contains("NEVER_EXPORT_THIS"), "{format:?}");
        let batch = decode(format, doc.contents.as_bytes()).unwrap();
        assert_eq!(batch.connections.len(), 1, "{format:?}");
        let c = &batch.connections[0];
        assert_eq!(c.values["host"], "db.example.com", "{format:?}");
        assert_eq!(c.values["port"], "5433", "{format:?}");
        assert_eq!(c.values["database"], "sales", "{format:?}");
        assert_eq!(c.name, "R&D <production>", "{format:?}");
        assert_eq!(c.values["user"], "alice", "{format:?}");
        assert_eq!(c.agent_access, AgentAccess::None);
    }
}

#[test]
fn malformed_input_does_not_echo_secrets_in_errors() {
    for format in ClientFormat::ALL {
        let error = decode(format, b"NEVER_ECHO_THIS invalid file").unwrap_err();
        assert!(!error.contains("NEVER_ECHO_THIS"));
    }
}

#[test]
fn postgres_only_export_reports_other_drivers_instead_of_losing_them_silently() {
    let mut c = pg();
    c.driver = "mysql".into();
    assert!(encode(ClientFormat::PgAdmin, &[c]).is_err());
}

#[test]
fn binary_tableplus_property_lists_are_detected_and_parsed() {
    let value = serde_json::json!([{"ConnectionName":"Local", "Driver":"PostgreSQL", "DatabaseHost":"localhost", "DatabasePort":5432}]);
    let mut bytes = Vec::new();
    plist::to_writer_binary(&mut bytes, &value).unwrap();
    assert_eq!(
        sqlr_store::client_migration::detect(&bytes).unwrap(),
        ClientFormat::TablePlus
    );
    assert_eq!(
        decode(ClientFormat::TablePlus, &bytes).unwrap().connections[0].values["host"],
        "localhost"
    );
}

#[test]
fn discovery_uses_standard_paths_and_datagrip_merges_its_local_file() {
    let home = tempfile::tempdir().unwrap();
    let project = home.path().join("DataGripProjects/app/.idea");
    std::fs::create_dir_all(&project).unwrap();
    let shared = project.join("dataSources.xml");
    std::fs::write(
        &shared,
        r#"<project><component><data-source name="App" uuid="external">
        <driver-ref>postgresql</driver-ref><jdbc-url>jdbc:postgresql://db/app</jdbc-url>
        </data-source></component></project>"#,
    )
    .unwrap();
    std::fs::write(project.join("dataSources.local.xml"),r#"<project><component>
        <data-source uuid="external"><user-name>alice</user-name><password>NEVER_COPY_THIS</password></data-source>
        </component></project>"#).unwrap();
    let sources = sqlr_store::client_migration::discover(home.path());
    assert!(sources.iter().any(|s| s.path == shared));
    let (_, batch) = sqlr_store::client_migration::read_source(&shared).unwrap();
    assert_eq!(batch.connections[0].values["user"], "alice");
    assert!(batch.connections[0].id.is_empty());
    assert!(!format!("{batch:?}").contains("NEVER_COPY_THIS"));
}

#[test]
fn batch_import_is_idempotent_preserves_existing_secrets_and_renames_name_collisions() {
    use sqlr_core::Secrets;
    use sqlr_store::{ConnectionRepo, MemorySecretStore, SecretStore};
    let dir = tempfile::tempdir().unwrap();
    let secrets = MemorySecretStore::default();
    secrets.set("original", "password", "KEEP_THIS").unwrap();
    // MemorySecretStore is not cloned here: a shared test store verifies the keychain.
    let repo = ConnectionRepo::open(dir.path(), Box::new(secrets)).unwrap();
    let existing = pg();
    repo.save(&existing, &Secrets::default()).unwrap();
    let mut duplicate = existing.clone();
    duplicate.id = "duplicate".into();
    assert_eq!(repo.import_connections(&[duplicate]).unwrap(), 0);
    let mut new = existing.clone();
    new.id = "new".into();
    new.values.insert("database".into(), "another".into());
    new.values
        .insert("password".into(), "NEVER_SAVE_THIS".into());
    assert_eq!(repo.import_connections(&[new.clone()]).unwrap(), 1);
    assert_eq!(repo.import_connections(&[new]).unwrap(), 0);
    let saved = repo.list().unwrap();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[1].name, "R&D <production> (2)");
    assert_eq!(saved[0], existing);
    assert_eq!(saved[1].agent_access, AgentAccess::None);
    assert!(
        !std::fs::read_to_string(dir.path().join("connections.toml"))
            .unwrap()
            .contains("NEVER_SAVE_THIS")
    );
    // Original keychain data can still be read through the driver manifest.
    let manifest = sqlr_core::DriverManifest {
        id: "postgres".into(),
        name: "Postgres".into(),
        family: sqlr_core::Family::Relational,
        capabilities: Default::default(),
        fields: vec![sqlr_core::FieldSpec {
            key: "password".into(),
            label: "Password".into(),
            ty: sqlr_core::FieldType::Password,
            default: None,
            required: false,
            secret: true,
            group: "Main".into(),
            free_form: false,
        }],
        dialect: "postgres".into(),
        url_schemes: vec![],
        default_port: None,
        editing: None,
    };
    assert_eq!(
        repo.secrets_for(&existing, &manifest).unwrap().0["password"],
        "KEEP_THIS"
    );
}

#[test]
fn batch_import_validation_failure_leaves_file_unchanged() {
    use sqlr_store::{ConnectionRepo, MemorySecretStore};
    let dir = tempfile::tempdir().unwrap();
    let repo = ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
    let mut first = pg();
    first.id = "fresh".into();
    let mut invalid = first.clone();
    invalid.id.clear();
    invalid.values.insert("database".into(), "other".into());
    assert!(repo.import_connections(&[first, invalid]).is_err());
    assert!(repo.list().unwrap().is_empty());
}

#[test]
fn datagrip_export_uses_native_jdbc_syntax_without_userinfo() {
    let c = pg();
    let doc = encode(ClientFormat::Datagrip, std::slice::from_ref(&c)).unwrap();
    assert!(
        doc.contents
            .contains("jdbc:postgresql://db.example.com:5433/sales")
    );
    assert!(!doc.contents.contains("alice@"));
    let mut ms = c;
    ms.driver = "mssql".into();
    let doc = encode(ClientFormat::Datagrip, &[ms]).unwrap();
    assert!(
        doc.contents
            .contains("jdbc:sqlserver://db.example.com:5433;databaseName=sales")
    );
    let b = decode(ClientFormat::Datagrip, doc.contents.as_bytes()).unwrap();
    assert_eq!(b.connections[0].values["database"], "sales");
}

#[test]
fn unsupported_network_handlers_are_not_silently_dropped() {
    let b = decode(
        ClientFormat::Dbeaver,
        br#"{"connections":{"x":{"name":"Proxy","provider":"postgresql",
       "configuration":{"host":"db","handlers":{"proxy":{"enabled":true}}}}}}"#,
    )
    .unwrap();
    assert!(b.connections.is_empty());
    assert!(b.warnings.iter().any(|s| s.contains("network handler")));
}

#[test]
fn dbeaver_ssl_handlers_keep_verification_mode_and_certificate_paths() {
    let batch = decode(
        ClientFormat::Dbeaver,
        br#"{"connections":{"pg":{
      "name":"TLS", "provider":"postgresql", "configuration":{"host":"db",
      "auth-properties":{"userName":"alice","userPassword":"NEVER_COPY_THIS"},
      "handlers":{"postgre_ssl":{"enabled":true,"properties":{
        "ssl.method":"CERTIFICATES","ssl.sslMode":"verify-full","ssl.ca.cert":"/keys/ca.pem"
      }}}}}}}"#,
    )
    .unwrap();
    let c = &batch.connections[0];
    assert_eq!(c.values["ssl_mode"], "verify-full");
    assert_eq!(c.values["ssl_root_cert"], "/keys/ca.pem");
    assert_eq!(c.values["user"], "alice");
    let doc = encode(ClientFormat::Dbeaver, &batch.connections).unwrap();
    assert!(doc.contents.contains("postgre_ssl"));
    assert!(!doc.contents.contains("NEVER_COPY_THIS"));
    let roundtrip = decode(ClientFormat::Dbeaver, doc.contents.as_bytes()).unwrap();
    assert_eq!(
        roundtrip.connections[0].values["ssl_root_cert"],
        "/keys/ca.pem"
    );
}

#[test]
fn mysql_tls_modes_are_normalized_to_sqlr_driver_choices() {
    let b = decode(
        ClientFormat::Dbeaver,
        br#"{"connections":{"m":{"name":"MySQL", "provider":"mysql",
      "configuration":{"host":"db","properties":{"sslMode":"VERIFY_IDENTITY"}}}}}"#,
    )
    .unwrap();
    assert_eq!(b.connections[0].values["ssl_mode"], "verify_identity");
    let doc = encode(ClientFormat::Dbeaver, &b.connections).unwrap();
    let b = decode(ClientFormat::Dbeaver, doc.contents.as_bytes()).unwrap();
    assert_eq!(b.connections[0].values["ssl_mode"], "verify_identity");
}

#[test]
fn clipboard_urls_discard_embedded_credentials_without_losing_the_connection() {
    let b = decode(ClientFormat::TablePlus,b"postgresql://alice:NEVER_COPY_THIS@db.example/sales?password=NEVER_COPY_THIS&sslmode=require&name=Sales").unwrap();
    assert_eq!(b.connections.len(), 1);
    assert_eq!(b.connections[0].values["user"], "alice");
    assert_eq!(b.connections[0].values["ssl_mode"], "require");
    assert!(!format!("{b:?}").contains("NEVER_COPY_THIS"));
    let backup = encode(ClientFormat::Sqlr, &b.connections).unwrap();
    assert!(!backup.contents.contains("NEVER_COPY_THIS"));
}

#[test]
fn local_datagrip_tls_is_never_weakened_to_default_settings() {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("dataSources.xml");
    std::fs::write(
        &shared,
        r#"<data-sources><data-source name="TLS" uuid="external"><driver-ref>postgresql</driver-ref>
      <jdbc-url>jdbc:postgresql://db/app</jdbc-url></data-source></data-sources>"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("dataSources.local.xml"),
        r#"<data-sources><data-source uuid="external">
      <ssl-config enabled="true" /></data-source></data-sources>"#,
    )
    .unwrap();
    let (_, batch) = sqlr_store::client_migration::read_source(&shared).unwrap();
    assert!(batch.connections.is_empty());
    assert!(!batch.warnings.is_empty());
}

#[test]
fn sqlite_file_paths_and_ipv6_hosts_round_trip_through_portable_urls() {
    for format in [ClientFormat::Datagrip, ClientFormat::TablePlus] {
        let mut c = pg();
        c.driver = "sqlite".into();
        c.values.clear();
        c.values.insert("path".into(), "/tmp/my database.db".into());
        let doc = encode(format, &[c]).unwrap();
        let b = decode(format, doc.contents.as_bytes()).unwrap();
        assert_eq!(
            b.connections[0].values["path"], "/tmp/my database.db",
            "{format:?}"
        );
        let mut c = pg();
        c.values.insert("host".into(), "::1".into());
        let doc = encode(format, &[c]).unwrap();
        let b = decode(format, doc.contents.as_bytes()).unwrap();
        assert_eq!(b.connections[0].values["host"], "::1", "{format:?}");
    }
}

#[test]
fn credentials_are_exported_only_for_supported_formats_with_explicit_opt_in() {
    use sqlr_core::Secrets;
    use sqlr_store::client_migration::encode_with_credentials;
    let c = pg();
    let secrets = [(
        c.id.clone(),
        Secrets(
            [
                ("password".into(), "p@ss:/?#&".into()),
                ("ssh_password".into(), "tunnel-secret".into()),
            ]
            .into(),
        ),
    )]
    .into();
    let doc =
        encode_with_credentials(ClientFormat::Sqlr, std::slice::from_ref(&c), &secrets).unwrap();
    let json: serde_json::Value = serde_json::from_str(&doc.contents).unwrap();
    assert_eq!(json["credentials"][&c.id]["password"], "p@ss:/?#&");
    assert_eq!(json["credentials"][&c.id]["ssh_password"], "tunnel-secret");
    let doc = encode_with_credentials(ClientFormat::TablePlus, std::slice::from_ref(&c), &secrets)
        .unwrap();
    let url = url::Url::parse(doc.contents.trim()).unwrap();
    assert_eq!(url.password(), Some("p%40ss%3A%2F%3F%23&"));
    assert!(!doc.contents.contains("tunnel-secret"));
    assert!(
        !encode(ClientFormat::Sqlr, std::slice::from_ref(&c))
            .unwrap()
            .contents
            .contains("p@ss")
    );
    for format in [
        ClientFormat::PgAdmin,
        ClientFormat::Dbeaver,
        ClientFormat::Datagrip,
    ] {
        assert!(encode_with_credentials(format, std::slice::from_ref(&c), &secrets).is_err());
    }
}

#[test]
fn tabularis_and_beekeeper_exports_preserve_credentials_only_when_requested() {
    use sqlr_core::Secrets;
    use sqlr_store::client_migration::encode_with_credentials;
    for format in [ClientFormat::Tabularis, ClientFormat::Beekeeper] {
        let c = pg();
        let secrets = [(
            c.id.clone(),
            Secrets(
                [
                    ("password".into(), "db-secret".into()),
                    ("ssh_password".into(), "ssh-secret".into()),
                ]
                .into(),
            ),
        )]
        .into();
        let plain = encode(format, std::slice::from_ref(&c)).unwrap();
        assert_eq!(
            sqlr_store::client_migration::detect(plain.contents.as_bytes()).unwrap(),
            format
        );
        assert!(!plain.contents.contains("db-secret"));
        let doc = encode_with_credentials(format, &[c], &secrets).unwrap();
        let batch = decode(format, doc.contents.as_bytes()).unwrap();
        assert_eq!(batch.connections[0].values["host"], "db.example.com");
        assert_eq!(batch.connections[0].values["ssl_mode"], "verify-full");
        assert_eq!(batch.credentials[&0].0["password"], "db-secret");
        assert_eq!(batch.connections[0].agent_access, AgentAccess::None);
        assert!(!batch.connections[0].values.contains_key("password"));
    }
}

#[test]
fn tabularis_import_resolves_nested_groups_and_linked_ssh() {
    let bytes = br#"{"version":1,"groups":[{"id":"root","name":"Team"},{"id":"child","name":"Production","parent_id":"root"}],"connections":[{"id":"foreign","name":"Sales","group_id":"child","params":{"driver":"postgres","host":"db","database":["sales","other"],"username":"alice","ssh_enabled":true,"ssh_connection_id":"ssh"}}],"ssh_connections":[{"id":"ssh","name":"Bastion","host":"jump","port":22,"user":"deploy","auth_type":"ssh_key","key_file":"/key","key_passphrase":"ssh-secret"}]}"#;
    let batch = decode(ClientFormat::Tabularis, bytes).unwrap();
    assert_eq!(
        batch.connections[0].group.as_deref(),
        Some("Team/Production")
    );
    assert_eq!(batch.connections[0].values["ssh_auth"], "key");
    assert_eq!(batch.connections[0].values["ssh_host"], "jump");
    assert_eq!(batch.credentials[&0].0["ssh_key_passphrase"], "ssh-secret");
    assert!(!batch.warnings.is_empty());
}

#[test]
fn beekeeper_batch_export_writes_native_files_without_overwriting_existing_folders() {
    use sqlr_store::client_migration::{read_source, save_export};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("beekeeper");
    let first = pg();
    let mut second = first.clone();
    second.id = "second".into();
    second.name = "Other".into();
    second.values.insert("database".into(), "other".into());
    save_export(&path, ClientFormat::Beekeeper, &[first.clone(), second]).unwrap();
    assert_eq!(std::fs::read_dir(&path).unwrap().count(), 2);
    for file in std::fs::read_dir(&path).unwrap() {
        let file = file.unwrap().path();
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert!(json.is_object());
        assert_eq!(json["connectionType"], "postgresql");
        assert!(!json.as_object().unwrap().contains_key("password"));
        let (format, batch) = read_source(&file).unwrap();
        assert_eq!(format, ClientFormat::Beekeeper);
        assert_eq!(batch.connections.len(), 1);
    }
    assert!(save_export(&path, ClientFormat::Beekeeper, &[first]).is_err());
}

#[test]
fn discovers_tabularis_and_reads_beekeeper_database_without_ciphertext_credentials() {
    use sqlr_store::client_migration::{discover, read_source};
    let dir = tempfile::tempdir().unwrap();
    let tab = dir
        .path()
        .join("Library/Application Support/tabularis/connections.json");
    std::fs::create_dir_all(tab.parent().unwrap()).unwrap();
    std::fs::write(
        &tab,
        encode(ClientFormat::Tabularis, &[pg()]).unwrap().contents,
    )
    .unwrap();
    let bee = dir.path().join(".config/beekeeper-studio/app.db");
    std::fs::create_dir_all(bee.parent().unwrap()).unwrap();
    let db = rusqlite::Connection::open(&bee).unwrap();
    db.execute_batch("CREATE TABLE saved_connection (id INTEGER, workspaceId INTEGER, name TEXT, connectionType TEXT, host TEXT, port INTEGER, username TEXT, defaultDatabase TEXT, password TEXT); INSERT INTO saved_connection VALUES (1,-1,'Local','postgresql','db',5432,'alice','sales','CIPHERTEXT'),(2,1,'Cloud','postgresql','cloud',5432,'alice','cloud','CIPHERTEXT');").unwrap();
    let sources = discover(dir.path());
    assert!(sources.iter().any(|s| s.path == tab));
    assert!(sources.iter().any(|s| s.path == bee));
    let (format, batch) = read_source(&bee).unwrap();
    assert_eq!(format, ClientFormat::Beekeeper);
    assert_eq!(batch.connections.len(), 1);
    assert_eq!(batch.connections[0].values["database"], "sales");
    assert!(batch.credentials.is_empty());
    assert!(!format!("{batch:?}").contains("CIPHERTEXT"));
    assert!(!batch.warnings.is_empty());
}

#[test]
fn tabularis_ssh_credentials_and_groups_round_trip_without_leaking_when_disabled() {
    use sqlr_core::Secrets;
    use sqlr_store::client_migration::encode_with_credentials;
    let mut c = pg();
    c.values.extend([
        ("ssh_enabled".into(), "true".into()),
        ("ssh_host".into(), "jump".into()),
        ("ssh_auth".into(), "key".into()),
        ("ssh_key_path".into(), "/key".into()),
    ]);
    let credentials = [(
        c.id.clone(),
        Secrets([("ssh_key_passphrase".into(), "ssh-secret".into())].into()),
    )]
    .into();
    let doc = encode_with_credentials(
        ClientFormat::Tabularis,
        std::slice::from_ref(&c),
        &credentials,
    )
    .unwrap();
    let batch = decode(ClientFormat::Tabularis, doc.contents.as_bytes()).unwrap();
    assert_eq!(batch.connections[0].group, c.group);
    assert_eq!(batch.connections[0].values["ssh_auth"], "key");
    assert_eq!(batch.credentials[&0].0["ssh_key_passphrase"], "ssh-secret");
    assert!(
        decode(
            ClientFormat::Tabularis,
            encode(ClientFormat::Tabularis, &[c])
                .unwrap()
                .contents
                .as_bytes()
        )
        .unwrap()
        .credentials
        .is_empty()
    );
    let encrypted = br#"{"format":"tabularis-connections-encrypted","version":1,"encrypted":true,"ciphertext":"SECRET"}"#;
    let error = sqlr_store::client_migration::detect(encrypted).unwrap_err();
    assert!(error.contains("Encrypted"));
    assert!(!error.contains("SECRET"));
}

#[test]
fn beekeeper_preserves_ssh_agent_auth_and_excludes_bastion_connections() {
    let input = br#"[{"name":"Agent","connectionType":"postgresql","host":"db","sshEnabled":true,"sshMode":"agent","sshHost":"ssh"},{"name":"Bastion","connectionType":"postgresql","host":"other","sshEnabled":true,"sshHost":"ssh","sshBastionHost":"jump"}]"#;
    let batch = decode(ClientFormat::Beekeeper, input).unwrap();
    assert_eq!(batch.connections.len(), 1);
    assert_eq!(batch.connections[0].values["ssh_auth"], "agent");
    assert!(batch.warnings.iter().any(|s| s.contains("bastion")));
    let doc = encode(ClientFormat::Beekeeper, &batch.connections).unwrap();
    let json: serde_json::Value = serde_json::from_str(&doc.contents).unwrap();
    assert_eq!(json["sshMode"], "agent");
    assert!(encode(ClientFormat::Tabularis, &batch.connections).is_err());
}

#[cfg(unix)]
#[test]
fn credential_export_files_are_owner_only() {
    use sqlr_core::Secrets;
    use sqlr_store::client_migration::{encode_with_credentials, save_document};
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("backup.json");
    let c = pg();
    let credentials = [(
        c.id.clone(),
        Secrets([("password".into(), "secret".into())].into()),
    )]
    .into();
    save_document(
        &path,
        encode_with_credentials(ClientFormat::Sqlr, &[c], &credentials).unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
