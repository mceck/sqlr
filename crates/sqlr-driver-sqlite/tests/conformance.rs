//! The shared conformance suite against the in-process SQLite driver.

use sqlr_core::{ConnectionConfig, Driver, Secrets, Value};
use sqlr_driver_sqlite::SqliteDriver;
use sqlr_driver_tests::{Target, TypeCase, relational_suite};

#[tokio::test]
async fn sqlite_passes_the_conformance_suite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conformance.db").display().to_string();
    let mut target = Target::new(
        "sqlite",
        Box::new(move || {
            let path = path.clone();
            Box::pin(async move {
                SqliteDriver::new()
                    .connect(
                        &ConnectionConfig {
                            id: "t".into(),
                            name: "t".into(),
                            driver: "sqlite".into(),
                            group: None,
                            color: None,
                            values: [
                                ("path".to_string(), path),
                                ("create_if_missing".to_string(), "true".to_string()),
                            ]
                            .into(),
                            profile: None,
                            save_secrets: true,
                            agent_access: Default::default(),
                        },
                        &Secrets::default(),
                    )
                    .await
            })
        }),
    )
    .with_setup([
        "DROP TABLE IF EXISTS sqlr_conformance",
        "CREATE TABLE sqlr_conformance (n INTEGER PRIMARY KEY, t TEXT)",
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 25) \
         INSERT INTO sqlr_conformance SELECT n, 'row ' || n FROM c",
    ]);
    target.capabilities.server_cancel = true;
    target.capabilities.editable_results = true;
    target.slow_sql = Some(
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c) SELECT count(*) FROM c"
            .into(),
    );
    target.type_cases = vec![
        TypeCase {
            name: "integer".into(),
            sql: "SELECT 42".into(),
            expect: Value::Int(42),
        },
        TypeCase {
            name: "text".into(),
            sql: "SELECT 'héllo'".into(),
            expect: Value::Text("héllo".into()),
        },
        TypeCase {
            name: "blob".into(),
            sql: "SELECT x'00ff'".into(),
            expect: Value::Bytes(vec![0, 255]),
        },
    ];
    let report = relational_suite(&target).await;
    assert!(report.failed.is_empty(), "{report}");
    assert!(report.passed.len() >= 10, "{report}");
}
