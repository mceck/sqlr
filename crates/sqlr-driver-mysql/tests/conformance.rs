//! The shared conformance suite against MySQL and MariaDB (needs Docker).

mod support;

use sqlr_core::{Driver, Value};
use sqlr_driver_mysql::MysqlDriver;
use sqlr_driver_tests::{Target, TypeCase, relational_suite};
use support::Server;

async fn suite(server: Server) {
    let (_container, cfg, secrets) = support::start(server).await;
    let mut target = Target::new(
        "mysql",
        Box::new(move || {
            let (cfg, secrets) = (cfg.clone(), secrets.clone());
            Box::pin(async move { MysqlDriver::new().connect(&cfg, &secrets).await })
        }),
    )
    .with_setup([
        "DROP TABLE IF EXISTS sqlr_conformance",
        "CREATE TABLE sqlr_conformance (n INT PRIMARY KEY, t VARCHAR(50))",
        "INSERT INTO sqlr_conformance \
         WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 25) \
         SELECT n, CONCAT('row ', n) FROM c",
    ]);
    target.capabilities.server_cancel = true;
    target.capabilities.editable_results = true;
    target.slow_sql = Some("SELECT SLEEP(60)".into());
    target.type_cases = vec![
        TypeCase {
            name: "int".into(),
            sql: "SELECT CAST(42 AS SIGNED)".into(),
            expect: Value::Int(42),
        },
        TypeCase {
            name: "text".into(),
            sql: "SELECT 'héllo'".into(),
            expect: Value::Text("héllo".into()),
        },
        TypeCase {
            name: "null".into(),
            sql: "SELECT NULL".into(),
            expect: Value::Null,
        },
    ];
    let report = relational_suite(&target).await;
    assert!(report.failed.is_empty(), "{}: {report}", server.product());
}

#[tokio::test]
async fn mysql_passes_the_conformance_suite() {
    sqlr_testkit::skip_container_tests!("MYSQL");
    suite(Server::MySql).await;
}

#[tokio::test]
async fn mariadb_passes_the_conformance_suite() {
    sqlr_testkit::skip_container_tests!("MARIADB");
    suite(Server::MariaDb).await;
}
