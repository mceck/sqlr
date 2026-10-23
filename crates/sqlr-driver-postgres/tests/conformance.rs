//! The shared conformance suite against PostgreSQL (needs Docker).

mod support;

use sqlr_core::{Driver, TableRef, Value};
use sqlr_driver_postgres::PostgresDriver;
use sqlr_driver_tests::{Target, TypeCase, relational_suite};

#[tokio::test]
async fn postgres_passes_the_conformance_suite() {
    sqlr_testkit::skip_container_tests!();
    let (_container, cfg, secrets) = support::pg().await;
    let mut target = Target::new(
        "postgres",
        Box::new(move || {
            let (cfg, secrets) = (cfg.clone(), secrets.clone());
            Box::pin(async move { PostgresDriver::new().connect(&cfg, &secrets).await })
        }),
    )
    .with_setup([
        "DROP TABLE IF EXISTS public.sqlr_conformance",
        "CREATE TABLE public.sqlr_conformance (n integer PRIMARY KEY, t text)",
        "INSERT INTO public.sqlr_conformance SELECT g, 'row ' || g FROM generate_series(1, 25) g",
    ]);
    target.table = TableRef {
        database: None,
        schema: Some("public".into()),
        name: "sqlr_conformance".into(),
    };
    target.capabilities.server_cancel = true;
    target.capabilities.editable_results = true;
    target.slow_sql = Some("SELECT pg_sleep(60)".into());
    target.type_cases = vec![
        TypeCase {
            name: "int4".into(),
            sql: "SELECT 42::int4".into(),
            expect: Value::Int(42),
        },
        TypeCase {
            name: "text".into(),
            sql: "SELECT 'héllo'::text".into(),
            expect: Value::Text("héllo".into()),
        },
        TypeCase {
            name: "null".into(),
            sql: "SELECT NULL::int4".into(),
            expect: Value::Null,
        },
    ];
    let report = relational_suite(&target).await;
    assert!(report.failed.is_empty(), "{report}");
}
