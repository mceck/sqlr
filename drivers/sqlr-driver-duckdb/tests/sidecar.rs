//! The DuckDB sidecar executable over the protocol, as sqlr runs it once installed.

use sqlr_driver_sdk::{
    Cell, ConnectionConfig, Driver, Execution, FieldType, Node, NodeKind, NodePath, Secrets, Value,
};
use sqlr_sidecar::{SidecarDriver, SidecarSpec};

fn spec() -> SidecarSpec {
    SidecarSpec::new("duckdb", "0.1.0", env!("CARGO_BIN_EXE_sqlr-driver-duckdb"))
}

fn config(id: &str, path: &str) -> ConnectionConfig {
    ConnectionConfig {
        id: id.into(),
        name: id.into(),
        driver: "duckdb".into(),
        group: None,
        color: None,
        values: [("path".to_string(), path.to_string())].into(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    }
}

/// Every cell of a query's result, as text.
async fn texts(conn: &dyn sqlr_driver_sdk::Connection, sql: &str) -> Vec<Vec<String>> {
    let Execution::Rows(mut rows) = conn.relational().unwrap().execute(sql).await.unwrap() else {
        panic!("{sql} returns rows");
    };
    let mut out = Vec::new();
    while let Some(chunk) = rows.next_chunk(100).await.unwrap() {
        for r in 0..chunk.len() {
            out.push(
                (0..chunk.columns().len())
                    .map(|c| match chunk.cell(r, c) {
                        Cell::Text(s) => s.to_string(),
                        Cell::Int(i) => i.to_string(),
                        other => format!("{other:?}"),
                    })
                    .collect(),
            );
        }
    }
    rows.close().await.unwrap();
    out
}

#[tokio::test]
async fn the_sidecar_announces_its_form() {
    let (_driver, manifest) = SidecarDriver::probe(spec()).await.unwrap();
    assert_eq!(manifest.id, "duckdb");
    assert_eq!(manifest.url_schemes, ["duckdb"]);
    assert!(manifest.capabilities.transactions);
    assert!(manifest.capabilities.server_cancel);
    let path = manifest.fields.iter().find(|f| f.key == "path").unwrap();
    assert!(matches!(path.ty, FieldType::File { save: false, .. }));
    assert_eq!(path.default.as_deref(), Some(":memory:"));
    let read_only = manifest
        .fields
        .iter()
        .find(|f| f.key == "read_only")
        .unwrap();
    assert_eq!(read_only.ty, FieldType::Bool);
}

#[tokio::test]
async fn sessions_share_one_database_and_results_stream_in_chunks() {
    let (driver, _) = SidecarDriver::probe(spec()).await.unwrap();
    let cfg = config("mem", ":memory:");
    let explorer = driver.connect(&cfg, &Secrets::default()).await.unwrap();
    let editor = driver.connect(&cfg, &Secrets::default()).await.unwrap();
    assert_eq!(explorer.ping().await.unwrap().product, "DuckDB");

    let sql = editor.relational().unwrap();
    sql.execute("CREATE TABLE \"odd \"\"name\"(id INTEGER, label VARCHAR)")
        .await
        .unwrap();
    let Execution::Affected(n) = sql
        .execute_with(
            "INSERT INTO \"odd \"\"name\" SELECT range, ? || range FROM range(5)",
            &[Value::Text("row ".into())],
        )
        .await
        .unwrap()
    else {
        panic!("an insert reports its count");
    };
    assert_eq!(n, 5);

    // The explorer's session sees the editor's table: one in-memory database per connection.
    let tables = explorer
        .browse(&NodePath(vec!["main".into(), "tables".into()]))
        .await
        .unwrap();
    let table = tables.iter().find(|n| n.name == "odd \"name").unwrap();
    assert_eq!(table.kind, NodeKind::Table);

    // The dialect travels as data and still names the relation by schema and table.
    let select = explorer
        .relational()
        .unwrap()
        .dialect()
        .select_all(table, Some(3))
        .unwrap();
    assert_eq!(select, "SELECT * FROM \"main\".\"odd \"\"name\" LIMIT 3");

    let Execution::Rows(mut rows) = sql
        .execute("SELECT id, label FROM \"odd \"\"name\" ORDER BY id")
        .await
        .unwrap()
    else {
        panic!("a select returns rows");
    };
    let mut sizes = Vec::new();
    while let Some(chunk) = rows.next_chunk(2).await.unwrap() {
        sizes.push(chunk.len());
    }
    assert_eq!(sizes, [2, 2, 1]);
    rows.close().await.unwrap();

    let catalog = explorer
        .relational()
        .unwrap()
        .catalog("main")
        .await
        .unwrap();
    assert_eq!(catalog.relations[0].columns.len(), 2);
}

#[tokio::test]
async fn two_sessions_open_the_same_file_in_one_process() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.duckdb");
    let (driver, _) = SidecarDriver::probe(spec()).await.unwrap();
    let cfg = config("file", path.to_str().unwrap());
    let first = driver.connect(&cfg, &Secrets::default()).await.unwrap();
    let second = driver.connect(&cfg, &Secrets::default()).await.unwrap();
    first
        .relational()
        .unwrap()
        .execute("CREATE TABLE t AS SELECT 42 AS answer")
        .await
        .unwrap();
    assert_eq!(
        texts(second.as_ref(), "SELECT answer FROM t").await,
        [["42"]]
    );
    let node = Node {
        path: NodePath(vec!["main".into(), "tables".into(), "t".into()]),
        name: "t".into(),
        kind: NodeKind::Table,
        has_children: true,
        detail: None,
    };
    let columns = second.browse(&node.path).await.unwrap();
    assert_eq!(columns[0].name, "answer");
}

#[tokio::test]
async fn connect_errors_come_back_over_the_wire() {
    let (driver, _) = SidecarDriver::probe(spec()).await.unwrap();
    let error = driver
        .connect(&config("none", ""), &Secrets::default())
        .await
        .err()
        .unwrap();
    assert!(error.message.contains(":memory:"), "{}", error.message);
}
