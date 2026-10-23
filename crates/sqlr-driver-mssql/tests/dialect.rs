//! The dialect's generated SQL, run on SQL Server 2025.

mod support;

use sqlr_core::{Dialect, Execution, Value};
use sqlr_driver_mssql::MsDialect;
use support::server;

async fn first_cell(rel: &dyn sqlr_core::RelationalConn, sql: &str) -> String {
    let Execution::Rows(mut rows) = rel.execute(sql).await.unwrap() else {
        panic!("{sql} returned no rows");
    };
    let chunk = rows.next_chunk(10).await.unwrap().unwrap();
    Value::from_cell(chunk.cell(0, 0)).display().into_owned()
}

#[tokio::test]
async fn ddl_that_needs_a_batch_of_its_own_runs() {
    sqlr_testkit::skip_container_tests!();
    let (_c, conn) = server().await;
    let rel = conn.relational().unwrap();
    for sql in [
        "CREATE SCHEMA shop",
        "CREATE TABLE shop.t (id int primary key, v nvarchar(20))",
        "CREATE VIEW shop.v AS SELECT id FROM shop.t",
    ] {
        rel.execute(sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
    }
    assert_eq!(first_cell(rel, "SELECT COUNT(*) FROM shop.v").await, "0");
}

#[tokio::test]
async fn upsert_toggles_literals_and_wrapping_work_on_the_server() {
    sqlr_testkit::skip_container_tests!();
    let (_c, conn) = server().await;
    let rel = conn.relational().unwrap();
    let d = MsDialect;
    rel.execute("CREATE TABLE up (id int primary key, v nvarchar(20))")
        .await
        .unwrap();
    let merge = d
        .upsert_row_sql(
            "[dbo].[up]",
            &["id".into(), "v".into()],
            &["@P1".into(), "@P2".into()],
            &["id".into()],
            &["v".into()],
        )
        .unwrap();
    for v in ["a", "città"] {
        let done = rel
            .execute_with(&merge, &[Value::Int(1), Value::Text(v.into())])
            .await
            .unwrap();
        assert!(matches!(done, Execution::Affected(1)));
    }
    assert_eq!(first_cell(rel, "SELECT v FROM up").await, "città");
    assert_eq!(first_cell(rel, "SELECT COUNT(*) FROM up").await, "1");

    let (off, on) = d.constraints_toggle_sql().unwrap();
    rel.execute(&off[0]).await.unwrap();
    rel.execute(&on[0]).await.unwrap();

    let literal = format!("SELECT {}", d.string_literal("città"));
    assert_eq!(first_cell(rel, &literal).await, "città");

    let wrapped = d.wrap_where("SELECT * FROM up ORDER BY id", "id = 1");
    assert_eq!(first_cell(rel, &wrapped).await, "1");
}

#[tokio::test]
async fn explain_returns_the_estimated_plan() {
    sqlr_testkit::skip_container_tests!();
    let (_c, conn) = server().await;
    let rel = conn.relational().unwrap();
    rel.execute("CREATE TABLE up (id int primary key)")
        .await
        .unwrap();
    let plan = rel
        .explain("SELECT * FROM up WHERE id = 1", false)
        .await
        .unwrap();
    assert!(plan.root.cost.is_some());
    assert!(
        plan.root.children.iter().any(|n| n.kind.contains("Seek")),
        "{plan:#?}"
    );
    // The session is back to normal afterwards.
    assert_eq!(first_cell(rel, "SELECT COUNT(*) FROM up").await, "0");
    assert!(rel.explain("DELETE FROM up", false).await.is_err());
}
