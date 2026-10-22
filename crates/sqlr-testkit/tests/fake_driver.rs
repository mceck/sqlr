use std::time::Duration;

use sqlr_core::{ConnectionConfig, Driver, ErrorKind, Execution, Secrets};
use sqlr_testkit::{FakeDriver, FakeScript};

fn cfg() -> ConnectionConfig {
    ConnectionConfig {
        id: "1".into(),
        name: "t".into(),
        driver: "fake".into(),
        group: None,
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: true,
    }
}

async fn open(driver: &FakeDriver) -> Box<dyn sqlr_core::RowStream> {
    let conn = driver.connect(&cfg(), &Secrets::default()).await.unwrap();
    match conn
        .relational()
        .unwrap()
        .execute("SELECT 1")
        .await
        .unwrap()
    {
        Execution::Rows(s) => s,
        Execution::Affected(_) => panic!("expected rows"),
    }
}

#[tokio::test]
async fn fake_streams_rows_in_chunks() {
    let driver = FakeDriver::new(FakeScript {
        rows: 1200,
        ..Default::default()
    });
    let mut stream = open(&driver).await;
    let mut lens = vec![];
    while let Some(c) = stream.next_chunk(500).await.unwrap() {
        lens.push(c.len());
    }
    assert_eq!(lens, vec![500, 500, 200]);
    assert_eq!(driver.closed_streams(), 0);
    stream.close().await.unwrap();
    stream.close().await.unwrap();
    assert_eq!(driver.closed_streams(), 1);
    assert_eq!(driver.executed(), vec!["SELECT 1".to_string()]);
}

#[tokio::test]
async fn fake_blocks_until_cancelled() {
    let driver = FakeDriver::new(FakeScript {
        rows: 10,
        block_on_fetch: true,
        ..Default::default()
    });
    let conn = driver.connect(&cfg(), &Secrets::default()).await.unwrap();
    let mut stream = match conn.relational().unwrap().execute("q").await.unwrap() {
        Execution::Rows(s) => s,
        _ => unreachable!(),
    };
    let fetch = tokio::spawn(async move { stream.next_chunk(5).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    conn.cancel_handle().cancel().await.unwrap();
    let res = tokio::time::timeout(Duration::from_secs(1), fetch)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(res.unwrap_err().kind, ErrorKind::Cancelled);
}

#[tokio::test]
async fn cancel_before_fetch_is_not_lost() {
    let driver = FakeDriver::new(FakeScript {
        rows: 10,
        block_on_fetch: true,
        ..Default::default()
    });
    let conn = driver.connect(&cfg(), &Secrets::default()).await.unwrap();
    let mut stream = match conn.relational().unwrap().execute("q").await.unwrap() {
        Execution::Rows(s) => s,
        _ => unreachable!(),
    };
    conn.cancel_handle().cancel().await.unwrap();
    let res = tokio::time::timeout(Duration::from_secs(1), stream.next_chunk(5))
        .await
        .unwrap();
    assert_eq!(res.unwrap_err().kind, ErrorKind::Cancelled);
}

#[tokio::test]
async fn cancel_before_execute_does_not_affect_next_stream() {
    let driver = FakeDriver::new(FakeScript {
        rows: 10,
        ..Default::default()
    });
    let conn = driver.connect(&cfg(), &Secrets::default()).await.unwrap();
    conn.cancel_handle().cancel().await.unwrap();
    let mut stream = match conn.relational().unwrap().execute("q").await.unwrap() {
        Execution::Rows(s) => s,
        _ => unreachable!(),
    };
    assert_eq!(stream.next_chunk(5).await.unwrap().unwrap().len(), 5);
}

#[tokio::test]
async fn fake_describes_scripted_tables_and_records_params() {
    use sqlr_core::{TableColumn, TableInfo, TableRef, Value};
    let info = TableInfo {
        table: TableRef {
            database: None,
            schema: Some("public".into()),
            name: "users".into(),
        },
        columns: vec![TableColumn {
            name: "id".into(),
            type_name: "int8".into(),
            nullable: false,
            default: None,
            generated: false,
        }],
        primary_key: vec!["id".into()],
        unique_keys: Vec::new(),
    };
    let driver = FakeDriver::new(FakeScript {
        affected: Some(1),
        tables: vec![info.clone()],
        ..Default::default()
    });
    let conn = driver.connect(&cfg(), &Secrets::default()).await.unwrap();
    let rel = conn.relational().unwrap();
    assert_eq!(rel.describe(&info.table).await.unwrap(), info);
    let missing = TableRef {
        name: "nope".into(),
        ..info.table.clone()
    };
    assert_eq!(
        rel.describe(&missing).await.unwrap_err().kind,
        ErrorKind::Other
    );
    let params = vec![Value::Int(1), Value::Text("a".into()), Value::Null];
    match rel
        .execute_with("UPDATE users SET x = ? WHERE id = ?", &params)
        .await
    {
        Ok(Execution::Affected(1)) => {}
        _ => panic!("expected Affected(1)"),
    }
    assert_eq!(
        driver.executed_with(),
        [("UPDATE users SET x = ? WHERE id = ?".to_string(), params)]
    );
    assert_eq!(driver.executed(), ["UPDATE users SET x = ? WHERE id = ?"]);
}
