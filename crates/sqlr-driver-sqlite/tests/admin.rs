//! Administration against a SQLite file: the database's properties
//! (PRAGMAs and an integrity check) and its table statistics. SQLite has no users or
//! sessions.

use std::sync::Arc;

use sqlr_core::{Connection, ConnectionConfig, Driver, ErrorKind, Execution, Secrets, TableRef};
use sqlr_driver_sqlite::SqliteDriver;

struct Env {
    _dir: tempfile::TempDir,
    conn: Arc<dyn Connection>,
}

impl Env {
    async fn new(setup: &[&str]) -> Self {
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
        let rel = conn.relational().unwrap();
        for sql in setup {
            match rel.execute(sql).await {
                Ok(Execution::Rows(mut s)) => {
                    while s.next_chunk(1000).await.unwrap().is_some() {}
                    s.close().await.unwrap();
                }
                Ok(Execution::Affected(_)) => {}
                Err(e) => panic!("{sql}\n{e:?}"),
            }
        }
        Self { _dir: dir, conn }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn row_estimates_come_from_analyze_when_present() {
    let env = Env::new(&[
        "CREATE TABLE t (id INTEGER PRIMARY KEY)",
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 500) \
         INSERT INTO t SELECT i FROM n",
        "ANALYZE",
        // Rows added after ANALYZE are not in its estimate.
        "INSERT INTO t (id) VALUES (1000), (1001)",
    ])
    .await;
    let admin = env.conn.relational().unwrap().admin().unwrap();
    let stats = admin.table_stats(Some("main")).await.unwrap();
    let t = stats.iter().find(|s| s.table.name == "t").unwrap();
    assert_eq!(t.rows_estimate, Some(500), "{t:?}");
    assert!(stats.iter().all(|s| s.table.name != "sqlite_stat1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn counting_rows_can_be_cancelled() {
    let env = Env::new(&["CREATE TABLE t (id INTEGER PRIMARY KEY)"]).await;
    let admin = env.conn.relational().unwrap().admin().unwrap();
    let cancel = admin
        .snapshot_cancel()
        .expect("statistics can be cancelled");
    cancel.cancel().await.unwrap();
    let e = admin.table_stats(Some("main")).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
    // The next read runs.
    assert_eq!(admin.table_stats(Some("main")).await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_cancel_does_not_stop_the_next_read() {
    let env = Env::new(&["CREATE TABLE t (id INTEGER PRIMARY KEY)"]).await;
    let admin = env.conn.relational().unwrap().admin().unwrap();
    let first = admin.snapshot_cancel().unwrap();
    assert_eq!(admin.table_stats(Some("main")).await.unwrap().len(), 1);
    // The first read is over when its cancel arrives.
    first.cancel().await.unwrap();
    let _second = admin.snapshot_cancel().unwrap();
    assert_eq!(admin.table_stats(Some("main")).await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn sqlite_properties_and_stats() {
    let mut setup = vec![
        "CREATE TABLE \"Order Lines\" (id INTEGER PRIMARY KEY, note TEXT)".to_string(),
        "CREATE INDEX by_note ON \"Order Lines\" (note)".into(),
        "CREATE TABLE empty (id INTEGER)".into(),
    ];
    setup.push(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 500) \
         INSERT INTO \"Order Lines\" SELECT i, printf('note %d', i) FROM n"
            .into(),
    );
    let refs: Vec<&str> = setup.iter().map(String::as_str).collect();
    let env = Env::new(&refs).await;
    let admin = env
        .conn
        .relational()
        .unwrap()
        .admin()
        .expect("admin support");

    let support = admin.supports();
    assert!(support.properties && support.statistics);
    assert!(!support.roles && !support.grants && !support.sessions && !support.locks);
    assert_eq!(
        admin.roles().await.unwrap_err().kind,
        ErrorKind::Unsupported
    );

    let properties = admin.properties().await.unwrap();
    let value = |name: &str| {
        properties
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("{name} in {properties:?}"))
            .value
            .clone()
    };
    assert_eq!(value("page_size"), "4096");
    assert_eq!(value("journal_mode"), "delete");
    assert_eq!(value("integrity_check"), "ok");
    assert!(value("page_count").parse::<i64>().unwrap() > 1);

    let sizes = admin.database_sizes().await.unwrap();
    assert!(
        sizes
            .iter()
            .any(|(name, bytes)| name == "main" && *bytes > 4096),
        "{sizes:?}"
    );

    let stats = admin.table_stats(Some("main")).await.unwrap();
    let lines = stats
        .iter()
        .find(|s| s.table.name == "Order Lines")
        .expect("the table");
    assert_eq!(
        lines.table,
        TableRef {
            database: None,
            schema: Some("main".into()),
            name: "Order Lines".into(),
        }
    );
    assert_eq!(lines.rows_estimate, Some(500));
    // dbstat is compiled into the bundled SQLite: sizes are exact.
    let (total, table, index) = (
        lines.total_bytes.unwrap(),
        lines.table_bytes.unwrap(),
        lines.index_bytes.unwrap(),
    );
    assert!(
        table > 0 && index > 0 && total == table + index,
        "{lines:?}"
    );
    let empty = stats.iter().find(|s| s.table.name == "empty").unwrap();
    assert_eq!(empty.rows_estimate, Some(0));
    assert!(stats.iter().all(|s| !s.table.name.starts_with("sqlite_")));
    // Without a schema, every attached database's tables.
    assert_eq!(admin.table_stats(None).await.unwrap().len(), stats.len());
}
