//! Live tests: they need a reachable SAP HANA and are ignored by default.
//!
//! ```text
//! SQLR_HANA_URL=hana://host:39041              # host:port; optional ?tenant=HXE&ca=/path/ca.pem
//!                                              #   &skip_verify=1&pin=1&tenant_port=39041&tls=0
//! SQLR_HANA_USER=... SQLR_HANA_PASSWORD=...
//! cargo test -p sqlr-driver-hana --test hana -- --ignored
//! ```

use sqlr_driver_hana::HanaDriver;
use sqlr_driver_sdk::{ConnectionConfig, Driver, Secrets, TableRef, Value};
use sqlr_driver_tests::{Target, TypeCase, relational_suite};

/// The connection described by the environment, or `None` (with a printed reason).
fn from_env() -> Option<(ConnectionConfig, Secrets)> {
    let url = std::env::var("SQLR_HANA_URL").ok()?;
    let user = std::env::var("SQLR_HANA_USER").ok()?;
    let password = std::env::var("SQLR_HANA_PASSWORD").ok()?;
    let rest = url.strip_prefix("hana://")?;
    let (authority, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (host, port) = authority.rsplit_once(':')?;
    let mut values: std::collections::BTreeMap<String, String> = [
        ("host", host),
        ("port", port),
        ("user", user.as_str()),
        ("tls", "true"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=')?;
        match k {
            "tenant" => values.insert("database".into(), v.into()),
            "ca" => values.insert("tls_ca_file".into(), v.into()),
            "skip_verify" => values.insert("tls_skip_verify".into(), (v == "1").to_string()),
            "pin" => values.insert("pin_redirect".into(), (v == "1").to_string()),
            "tenant_port" => values.insert("tenant_port".into(), v.into()),
            "tls" => values.insert("tls".into(), (v != "0").to_string()),
            _ => None,
        };
    }
    Some((
        ConnectionConfig {
            id: "hana".into(),
            name: "hana".into(),
            driver: "hana".into(),
            group: None,
            color: None,
            values,
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        },
        Secrets([("password".to_string(), password)].into()),
    ))
}

#[tokio::test]
#[ignore = "needs a reachable SAP HANA (SQLR_HANA_URL, SQLR_HANA_USER, SQLR_HANA_PASSWORD)"]
async fn hana_conformance() {
    let Some((cfg, secrets)) = from_env() else {
        eprintln!("skipped: SQLR_HANA_URL, SQLR_HANA_USER or SQLR_HANA_PASSWORD is not set");
        return;
    };
    let mut target = Target::new(
        "hana",
        Box::new(move || {
            let (cfg, secrets) = (cfg.clone(), secrets.clone());
            Box::pin(async move { HanaDriver::new().connect(&cfg, &secrets).await })
        }),
    )
    .with_setup([
        "DO BEGIN DECLARE n INT; \
         SELECT COUNT(*) INTO n FROM SYS.TABLES WHERE SCHEMA_NAME = CURRENT_SCHEMA AND TABLE_NAME = 'sqlr_conformance'; \
         IF :n > 0 THEN EXEC 'DROP TABLE \"sqlr_conformance\"'; END IF; END",
        "CREATE TABLE \"sqlr_conformance\" (\"n\" INTEGER PRIMARY KEY, \"t\" NVARCHAR(50))",
        "INSERT INTO \"sqlr_conformance\" SELECT GENERATED_PERIOD_START, 'row ' || GENERATED_PERIOD_START \
         FROM SERIES_GENERATE_INTEGER(1, 1, 26)",
    ]);
    target.table = TableRef {
        database: None,
        schema: None,
        name: "sqlr_conformance".into(),
    };
    target.capabilities.server_cancel = true;
    target.capabilities.editable_results = true;
    target.slow_sql = Some(
        "SELECT COUNT(*) FROM SERIES_GENERATE_INTEGER(1, 1, 1000000000) A, \
         SERIES_GENERATE_INTEGER(1, 1, 1000) B"
            .into(),
    );
    target.type_cases = vec![
        TypeCase {
            name: "integer".into(),
            sql: "SELECT 42 FROM DUMMY".into(),
            expect: Value::Int(42),
        },
        TypeCase {
            name: "decimal".into(),
            sql: "SELECT CAST('1234567890123456789012345678.0123456789' AS DECIMAL(38,10)) FROM DUMMY".into(),
            expect: Value::Text("1234567890123456789012345678.0123456789".into()),
        },
        TypeCase {
            name: "nvarchar".into(),
            sql: "SELECT N'日本語 è 🚀' FROM DUMMY".into(),
            expect: Value::Text("日本語 è 🚀".into()),
        },
        TypeCase {
            name: "varbinary".into(),
            sql: "SELECT x'DEADBEEF' FROM DUMMY".into(),
            expect: Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
        },
    ];
    let report = relational_suite(&target).await;
    assert!(report.failed.is_empty(), "{report}");
}
