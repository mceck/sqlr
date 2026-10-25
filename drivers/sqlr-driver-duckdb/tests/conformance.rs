use sqlr_driver_duckdb::DuckdbDriver;
use sqlr_driver_sdk::{ConnectionConfig, Driver, Secrets, Value};
use sqlr_driver_tests::{Target, TypeCase, relational_suite};

#[tokio::test]
async fn duckdb_passes_the_real_engine_conformance_suite() {
    let mut target = Target::new(
        "duckdb",
        Box::new(|| {
            Box::pin(async {
                DuckdbDriver::new()
                    .connect(
                        &ConnectionConfig {
                            id: "conformance".into(),
                            name: "conformance".into(),
                            driver: "duckdb".into(),
                            group: None,
                            color: None,
                            values: [("path".into(), ":memory:".into())].into(),
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
        "CREATE TABLE sqlr_conformance(n INTEGER PRIMARY KEY, t VARCHAR)",
        "INSERT INTO sqlr_conformance SELECT i, 'row ' || i FROM range(1,26) t(i)",
    ]);
    target.capabilities.server_cancel = true;
    target.slow_sql =
        Some("SELECT sum(a.i*b.i) FROM range(1000000000) a(i), range(1000000000) b(i)".into());
    target.type_cases = vec![
        TypeCase {
            name: "integer".into(),
            sql: "SELECT 42".into(),
            expect: Value::Int(42),
        },
        TypeCase {
            name: "boolean".into(),
            sql: "SELECT true".into(),
            expect: Value::Bool(true),
        },
        TypeCase {
            name: "hugeint".into(),
            sql: "SELECT 170141183460469231731687303715884105727::HUGEINT".into(),
            expect: Value::Text("170141183460469231731687303715884105727".into()),
        },
        TypeCase {
            name: "decimal".into(),
            sql: "SELECT 12345678901234567890.123456789::DECIMAL(38,9)".into(),
            expect: Value::Text("12345678901234567890.123456789".into()),
        },
        TypeCase {
            name: "text".into(),
            sql: "SELECT 'héllo'".into(),
            expect: Value::Text("héllo".into()),
        },
        TypeCase {
            name: "blob".into(),
            sql: "SELECT from_hex('00ff')".into(),
            expect: Value::Bytes(vec![0, 255]),
        },
    ];
    let report = relational_suite(&target).await;
    assert!(report.failed.is_empty(), "{report}");
    assert!(report.passed.len() >= 10, "{report}");
}
