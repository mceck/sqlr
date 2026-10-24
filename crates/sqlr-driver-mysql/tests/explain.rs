//! Explain against MySQL 8.4 and MariaDB 11.4.

mod support;

use sqlr_core::{Driver, ErrorKind};
use sqlr_driver_mysql::MysqlDriver;
use support::{Server, start};

async fn explain_on(server: Server) {
    let (_container, cfg, secrets) = start(server).await;
    let conn = MysqlDriver::new().connect(&cfg, &secrets).await.unwrap();
    let rel = conn.relational().unwrap();
    for sql in [
        "CREATE TABLE t (id INT PRIMARY KEY, a INT, KEY ia (a))",
        "INSERT INTO t VALUES (1, 1), (2, 2), (3, 3)",
    ] {
        rel.execute(sql).await.unwrap();
    }
    let plan = rel
        .explain("SELECT * FROM t WHERE a = 2", false)
        .await
        .unwrap();
    assert!(
        !plan.root.children.is_empty() || plan.root.relation.is_some(),
        "{plan:?}"
    );
    // MySQL runs a query for the analyzed plan; an update is refused, and says so.
    let analyzed = rel
        .explain("SELECT * FROM t WHERE a = 2", true)
        .await
        .unwrap();
    assert!(
        analyzed.execution_ms.is_some() || analyzed.root.time_ms.is_some(),
        "{analyzed:?}"
    );
    let e = rel.explain("UPDATE t SET a = 9", true).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unsupported);
    assert!(
        e.message.contains("only offered for queries"),
        "{}",
        e.message
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mysql84_explain_tree_parsed() {
    sqlr_testkit::skip_container_tests!("MYSQL");
    explain_on(Server::MySql).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mariadb114_explain_json_parsed() {
    sqlr_testkit::skip_container_tests!("MARIADB");
    explain_on(Server::MariaDb).await;
}
