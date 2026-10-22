//! Explain against SQLite files.

use sqlr_core::{Connection, ConnectionConfig, Driver, ErrorKind, Execution, Secrets, Value};
use sqlr_driver_sqlite::SqliteDriver;
use std::sync::Arc;

async fn open(setup: &str) -> (tempfile::TempDir, Arc<dyn Connection>) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ConnectionConfig {
        id: "t".into(),
        name: "test".into(),
        driver: "sqlite".into(),
        group: None,
        color: None,
        values: [
            ("path", dir.path().join("t.db").display().to_string()),
            ("create_if_missing", "true".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        profile: None,
        save_secrets: true,
    };
    let conn = SqliteDriver::new()
        .connect(&cfg, &Secrets::default())
        .await
        .unwrap();
    for sql in setup.split(";\n").map(str::trim).filter(|s| !s.is_empty()) {
        if let Execution::Rows(mut s) = conn.relational().unwrap().execute(sql).await.unwrap() {
            while s.next_chunk(100).await.unwrap().is_some() {}
        }
    }
    (dir, conn)
}

#[tokio::test]
async fn sqlite_explain_query_plan_parsed() {
    let (_dir, conn) =
        open("CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT);\nCREATE INDEX t_a ON t(a)").await;
    let rel = conn.relational().unwrap();
    let plan = rel
        .explain("SELECT * FROM t WHERE a = 'x'", false)
        .await
        .unwrap();
    let labels: Vec<&str> = plan
        .root
        .children
        .iter()
        .map(|c| c.label.as_str())
        .collect();
    assert!(
        labels
            .iter()
            .any(|l| l.contains("SEARCH") && l.contains("t_a")),
        "{labels:?}"
    );
    assert_eq!(plan.format, "text");
}

#[tokio::test]
async fn explain_unsupported_statement_message() {
    let (_dir, conn) = open("CREATE TABLE t (id INTEGER)").await;
    let rel = conn.relational().unwrap();
    for sql in [
        "CREATE TABLE u (a int)",
        "SELECT 1; SELECT 2",
        "PRAGMA foreign_keys",
    ] {
        let e = rel.explain(sql, false).await.unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsupported, "{sql}: {e:?}");
        assert!(e.message.starts_with("Explain"), "{}", e.message);
    }
    // SQLite has no ANALYZE, and says so.
    let e = rel.explain("SELECT * FROM t", true).await.unwrap_err();
    assert!(e.message.contains("no EXPLAIN ANALYZE"), "{}", e.message);
}

#[tokio::test]
async fn explaining_a_data_change_does_not_change_data() {
    let (_dir, conn) = open("CREATE TABLE t (id INTEGER);\nINSERT INTO t VALUES (1)").await;
    let rel = conn.relational().unwrap();
    rel.explain("UPDATE t SET id = 2", false).await.unwrap();
    let Execution::Rows(mut s) = rel.execute("SELECT id FROM t").await.unwrap() else {
        panic!("rows");
    };
    let chunk = s.next_chunk(10).await.unwrap().unwrap();
    assert_eq!(Value::from_cell(chunk.cell(0, 0)), Value::Int(1));
}
