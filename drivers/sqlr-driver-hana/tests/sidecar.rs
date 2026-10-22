//! The HANA sidecar executable over the protocol, without a HANA server.

use sqlr_driver_sdk::{ConnectionConfig, Driver, ErrorKind, FieldType, Secrets};
use sqlr_sidecar::{SidecarDriver, SidecarSpec};

fn spec() -> SidecarSpec {
    SidecarSpec::new("hana", "0.1.0", env!("CARGO_BIN_EXE_sqlr-driver-hana"))
}

fn config(values: &[(&str, &str)]) -> ConnectionConfig {
    ConnectionConfig {
        id: "h".into(),
        name: "h".into(),
        driver: "hana".into(),
        group: None,
        color: None,
        values: values
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        profile: None,
        save_secrets: true,
    }
}

#[tokio::test]
async fn the_sidecar_announces_its_form() {
    let (_driver, manifest) = SidecarDriver::probe(spec()).await.unwrap();
    assert_eq!(manifest.id, "hana");
    let keys: Vec<&str> = manifest.fields.iter().map(|f| f.key.as_str()).collect();
    for key in [
        "host",
        "port",
        "database",
        "user",
        "password",
        "tls",
        "tls_ca_file",
        "tls_skip_verify",
        "pin_redirect",
        "tenant_port",
    ] {
        assert!(keys.contains(&key), "{key} in {keys:?}");
    }
    let password = manifest
        .fields
        .iter()
        .find(|f| f.key == "password")
        .unwrap();
    assert!(password.secret);
    assert_eq!(password.ty, FieldType::Password);
    let tls: Vec<_> = manifest
        .fields
        .iter()
        .filter(|f| f.group == "TLS")
        .collect();
    assert_eq!(tls.len(), 3, "the TLS group");
    assert!(manifest.capabilities.server_cancel);
}

#[tokio::test]
async fn connect_errors_come_back_over_the_wire() {
    let (driver, _) = SidecarDriver::probe(spec()).await.unwrap();
    // Missing fields are a clear error, not a hang or a crash.
    let e = driver
        .connect(&config(&[]), &Secrets::default())
        .await
        .err()
        .unwrap();
    assert!(e.message.contains("host"), "{}", e.message);
    // Nothing listens on port 1.
    let e = driver
        .connect(
            &config(&[
                ("host", "127.0.0.1"),
                ("port", "1"),
                ("user", "u"),
                ("tls", "false"),
            ]),
            &Secrets([("password".to_string(), "hunter2".to_string())].into()),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(e.kind, ErrorKind::Connection, "{e:?}");
    assert!(
        !e.message.contains("hunter2"),
        "the password never reaches an error"
    );
    // The pin without the tenant's port says what is missing.
    let e = driver
        .connect(
            &config(&[
                ("host", "127.0.0.1"),
                ("port", "30013"),
                ("user", "u"),
                ("database", "HXE"),
                ("pin_redirect", "true"),
            ]),
            &Secrets::default(),
        )
        .await
        .err()
        .unwrap();
    assert!(e.message.contains("SQL port"), "{}", e.message);
}
