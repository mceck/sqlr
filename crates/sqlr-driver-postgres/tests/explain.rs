//! Explain against PostgreSQL 17.

mod support;

use sqlr_core::{Driver, ErrorKind, Execution, Value};
use sqlr_driver_postgres::PostgresDriver;
use support::pg;

async fn first_value(rel: &dyn sqlr_core::RelationalConn, sql: &str) -> Value {
    let Execution::Rows(mut s) = rel.execute(sql).await.unwrap() else {
        panic!("rows");
    };
    let chunk = s.next_chunk(10).await.unwrap().unwrap();
    let v = Value::from_cell(chunk.cell(0, 0));
    s.close().await.unwrap();
    v
}

#[tokio::test]
async fn pg_explain_json_parsed() {
    sqlr_testkit::skip_container_tests!();
    let (_c, cfg, sec) = pg().await;
    let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    let rel = conn.relational().unwrap();
    rel.execute("CREATE TABLE t (id int PRIMARY KEY, a text)")
        .await
        .unwrap();
    rel.execute("INSERT INTO t SELECT g, 'x' FROM generate_series(1, 200) g")
        .await
        .unwrap();

    let plan = rel
        .explain("SELECT * FROM t WHERE id < 50", false)
        .await
        .unwrap();
    assert_eq!(plan.format, "json");
    assert!(plan.root.cost.is_some());
    assert!(
        plan.execution_ms.is_none(),
        "a plain explain does not run the query"
    );

    let analyzed = rel
        .explain("SELECT * FROM t WHERE id < 50", true)
        .await
        .unwrap();
    assert!(analyzed.execution_ms.is_some());
    assert_eq!(analyzed.root.rows_actual, Some(49.0));
    assert!(!analyzed.hottest(2).is_empty());

    let e = rel.explain("SELECT $1", false).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unsupported);
    let e = rel
        .explain("CREATE TABLE u (a int)", false)
        .await
        .unwrap_err();
    assert!(e.message.starts_with("Explain"), "{}", e.message);
}

#[tokio::test]
async fn explain_analyze_update_rolls_back() {
    sqlr_testkit::skip_container_tests!();
    let (_c, cfg, sec) = pg().await;
    let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    let rel = conn.relational().unwrap();
    rel.execute("CREATE TABLE t (id int PRIMARY KEY, n int)")
        .await
        .unwrap();
    rel.execute("INSERT INTO t VALUES (1, 10)").await.unwrap();
    let plan = rel
        .explain("UPDATE t SET n = 99 WHERE id = 1", true)
        .await
        .unwrap();
    assert!(plan.execution_ms.is_some(), "the statement did run");
    assert_eq!(
        first_value(rel, "SELECT n FROM t WHERE id = 1").await,
        Value::Int(10)
    );
}
