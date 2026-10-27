use sqlr_driver_duckdb::DuckdbDriver;
use sqlr_driver_sdk::{
    Cell, Connection, ConnectionConfig, Driver, Execution, NodePath, Secrets, TableRef, Value,
};
use std::sync::Arc;

fn config(path: &str, read_only: bool) -> ConnectionConfig {
    ConnectionConfig {
        id: "test".into(),
        name: "test".into(),
        driver: "duckdb".into(),
        group: None,
        color: None,
        values: [
            ("path".into(), path.into()),
            ("read_only".into(), read_only.to_string()),
        ]
        .into(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    }
}
async fn connect(path: &str, read_only: bool) -> Arc<dyn Connection> {
    DuckdbDriver::new()
        .connect(&config(path, read_only), &Secrets::default())
        .await
        .unwrap()
}
async fn rows(conn: &dyn Connection, sql: &str) -> Vec<Vec<Value>> {
    let Execution::Rows(mut stream) = conn.relational().unwrap().execute(sql).await.unwrap() else {
        panic!("expected rows")
    };
    let mut result = Vec::new();
    while let Some(chunk) = stream.next_chunk(7).await.unwrap() {
        for r in 0..chunk.len() {
            result.push(
                (0..chunk.columns().len())
                    .map(|c| Value::from_cell(chunk.cell(r, c)))
                    .collect(),
            );
        }
    }
    result
}
#[tokio::test]
async fn real_embedded_connection_binds_values_and_chunks_results() {
    let conn = connect(":memory:", false).await;
    assert_eq!(conn.ping().await.unwrap().product, "DuckDB");
    let rel = conn.relational().unwrap();
    assert!(matches!(
        rel.execute("CREATE TABLE t(i BIGINT, s VARCHAR, b BLOB)")
            .await
            .unwrap(),
        Execution::Affected(0)
    ));
    assert!(matches!(
        rel.execute_with(
            "INSERT INTO t VALUES (?, ?, ?)",
            &[
                Value::Int(42),
                Value::Text("héllo'".into()),
                Value::Bytes(vec![0, 255])
            ]
        )
        .await
        .unwrap(),
        Execution::Affected(1)
    ));
    assert_eq!(
        rows(&*conn, "SELECT * FROM t").await,
        vec![vec![
            Value::Int(42),
            Value::Text("héllo'".into()),
            Value::Bytes(vec![0, 255])
        ]]
    );
    let Execution::Rows(mut stream) = rel
        .execute("SELECT range AS n FROM range(100003)")
        .await
        .unwrap()
    else {
        panic!()
    };
    let mut total = 0;
    while let Some(chunk) = stream.next_chunk(133).await.unwrap() {
        assert!(chunk.len() <= 133);
        assert_eq!(chunk.cell(0, 0), Cell::Int(total));
        total += chunk.len() as i64;
    }
    assert_eq!(total, 100003);
}
#[tokio::test]
async fn precise_types_nulls_and_empty_results() {
    let conn = connect(":memory:", false).await;
    let result=rows(&*conn,"SELECT 170141183460469231731687303715884105727::HUGEINT, 12345678901234567890.123456789::DECIMAL(38,9), DATE '2024-02-29', TIME '12:34:56.123456', TIMESTAMP '2024-02-29 12:34:56.123456', NULL::INTEGER, true, 1.25::DOUBLE").await;
    assert_eq!(
        result[0],
        vec![
            Value::Text("170141183460469231731687303715884105727".into()),
            Value::Text("12345678901234567890.123456789".into()),
            Value::Text("2024-02-29".into()),
            Value::Text("12:34:56.123456".into()),
            Value::Text("2024-02-29 12:34:56.123456".into()),
            Value::Null,
            Value::Bool(true),
            Value::Float(1.25)
        ]
    );
    let Execution::Rows(mut stream) = conn
        .relational()
        .unwrap()
        .execute("SELECT 1::INTEGER AS n WHERE false")
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(stream.columns()[0].name, "n");
    assert!(stream.next_chunk(10).await.unwrap().is_none());
}
#[tokio::test]
async fn persistence_read_only_transactions_and_catalogs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native.duckdb");
    let path = path.to_str().unwrap();
    {
        let conn = connect(path, false).await;
        let rel = conn.relational().unwrap();
        rel.execute("CREATE SCHEMA \"a'b\"").await.unwrap();
        rel.execute("CREATE TABLE \"a'b\".\"weird\"\"table\" (id INTEGER PRIMARY KEY, code VARCHAR UNIQUE NOT NULL, d DECIMAL(9,2) DEFAULT 1.25)").await.unwrap();
        rel.execute("CREATE VIEW \"a'b\".v AS SELECT * FROM \"a'b\".\"weird\"\"table\"")
            .await
            .unwrap();
        rel.execute("BEGIN").await.unwrap();
        rel.execute("INSERT INTO \"a'b\".\"weird\"\"table\" VALUES (1,'x',2.50)")
            .await
            .unwrap();
        rel.execute("ROLLBACK").await.unwrap();
        assert_eq!(
            rows(&*conn, "SELECT count(*) FROM \"a'b\".\"weird\"\"table\"").await[0][0],
            Value::Int(0)
        );
        let info = rel
            .describe(&TableRef {
                database: None,
                schema: Some("a'b".into()),
                name: "weird\"table".into(),
            })
            .await
            .unwrap();
        assert_eq!(info.primary_key, ["id"]);
        assert_eq!(info.unique_keys, vec![vec!["code"]]);
        assert!(!info.columns[0].nullable);
        let cat = rel.catalog("a'b").await.unwrap();
        assert_eq!(cat.relations.len(), 2);
        assert_eq!(cat.relations[1].columns[2].type_name, "DECIMAL(9,2)");
        let schemas = conn.browse(&NodePath::root()).await.unwrap();
        assert!(schemas.iter().any(|n| n.name == "a'b"));
        let folders = conn.browse(&NodePath(vec!["a'b".into()])).await.unwrap();
        assert_eq!(folders.len(), 2);
        let tables = conn
            .browse(&NodePath(vec!["a'b".into(), "tables".into()]))
            .await
            .unwrap();
        assert_eq!(tables[0].name, "weird\"table");
        assert_eq!(conn.browse(&tables[0].path).await.unwrap().len(), 3);
    }
    let conn = connect(path, true).await;
    assert!(
        conn.relational()
            .unwrap()
            .execute("CREATE TABLE forbidden(i INTEGER)")
            .await
            .is_err()
    );
    assert_eq!(
        rows(&*conn, "SELECT count(*) FROM \"a'b\".\"weird\"\"table\"").await[0][0],
        Value::Int(0)
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_interrupts_and_connection_recovers() {
    let conn = connect(":memory:", false).await;
    let task_conn = conn.clone();
    let task = tokio::spawn(async move {
        match task_conn
            .relational()
            .unwrap()
            .execute("SELECT sum(a.i*b.i) FROM range(1000000000) a(i), range(1000000000) b(i)")
            .await
        {
            Err(e) => Err(e),
            Ok(Execution::Rows(mut s)) => s.next_chunk(1).await.map(|_| ()),
            Ok(_) => Ok(()),
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    conn.cancel_handle().cancel().await.unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind, sqlr_driver_sdk::ErrorKind::Cancelled);
    assert_eq!(rows(&*conn, "SELECT 42").await[0][0], Value::Int(42));
}

#[tokio::test]
async fn nested_values_uuid_and_infinite_timestamps_remain_readable() {
    let conn = connect(":memory:", false).await;
    let result=rows(&*conn,"SELECT [1, NULL, 3], {'a': 42, 'b': 'text'}, UUID '550e8400-e29b-41d4-a716-446655440000', TIMESTAMP 'infinity', TIMESTAMP '-infinity'").await;
    assert_eq!(result[0][0], Value::Text("[1, , 3]".into()));
    assert!(
        matches!(&result[0][1], Value::Text(text) if text.contains("a") && text.contains("42") && text.contains("text"))
    );
    assert_eq!(
        result[0][2],
        Value::Text("550e8400-e29b-41d4-a716-446655440000".into())
    );
    assert_eq!(result[0][3], Value::Text("infinity".into()));
    assert_eq!(result[0][4], Value::Text("-infinity".into()));
}

#[tokio::test]
async fn generated_columns_are_described_as_generated() {
    let conn = connect(":memory:", false).await;
    conn.relational()
        .unwrap()
        .execute("CREATE TABLE generated_test (a INTEGER, b INTEGER GENERATED ALWAYS AS (a + 1))")
        .await
        .unwrap();
    let info = conn
        .relational()
        .unwrap()
        .describe(&TableRef {
            database: None,
            schema: Some("main".into()),
            name: "generated_test".into(),
        })
        .await
        .unwrap();
    assert!(info.columns[1].generated);
}

#[tokio::test]
async fn editor_and_explorer_sessions_share_database_with_independent_transactions() {
    let driver = DuckdbDriver::new();
    for path in [":memory:"] {
        let cfg = config(path, false);
        let explorer = driver.connect(&cfg, &Secrets::default()).await.unwrap();
        let editor = driver.connect(&cfg, &Secrets::default()).await.unwrap();
        explorer
            .relational()
            .unwrap()
            .execute("CREATE TABLE shared_session(i INTEGER)")
            .await
            .unwrap();
        editor
            .relational()
            .unwrap()
            .execute("INSERT INTO shared_session VALUES (42)")
            .await
            .unwrap();
        assert_eq!(
            rows(&*explorer, "SELECT * FROM shared_session").await[0][0],
            Value::Int(42)
        );
        editor.relational().unwrap().execute("BEGIN").await.unwrap();
        editor
            .relational()
            .unwrap()
            .execute("INSERT INTO shared_session VALUES (43)")
            .await
            .unwrap();
        assert_eq!(
            rows(&*explorer, "SELECT count(*) FROM shared_session").await[0][0],
            Value::Int(1)
        );
        editor
            .relational()
            .unwrap()
            .execute("ROLLBACK")
            .await
            .unwrap();
        drop(explorer);
        let later = driver.connect(&cfg, &Secrets::default()).await.unwrap();
        assert_eq!(
            rows(&*later, "SELECT * FROM shared_session").await[0][0],
            Value::Int(42)
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.duckdb");
    let cfg = config(path.to_str().unwrap(), false);
    let first = driver.connect(&cfg, &Secrets::default()).await.unwrap();
    let second = driver.connect(&cfg, &Secrets::default()).await.unwrap();
    first
        .relational()
        .unwrap()
        .execute("CREATE TABLE shared_file(i INTEGER)")
        .await
        .unwrap();
    assert!(rows(&*second, "SELECT * FROM shared_file").await.is_empty());
}

#[tokio::test]
async fn duckdb_query_forms_return_rows_and_returning_keeps_user_columns() {
    let conn = connect(":memory:", false).await;
    assert_eq!(rows(&*conn, "FROM range(3)").await.len(), 3);
    assert_eq!(rows(&*conn, "SUMMARIZE SELECT 1 AS n").await.len(), 1);
    assert_eq!(
        rows(
            &*conn,
            "PIVOT (SELECT 'x' AS name, 1 AS amount) ON name USING sum(amount)"
        )
        .await[0][0],
        Value::Text("1".into())
    );
    assert!(
        !rows(&*conn, "/* leading comment */ CALL\npragma_version()")
            .await
            .is_empty()
    );
    conn.relational()
        .unwrap()
        .execute("CREATE TABLE returning_count(i INTEGER)")
        .await
        .unwrap();
    assert_eq!(
        rows(
            &*conn,
            "INSERT INTO returning_count VALUES(1) RETURNING i AS \"Count\""
        )
        .await[0][0],
        Value::Int(1)
    );
}

#[tokio::test]
async fn catalog_and_ping_do_not_end_a_live_stream() {
    let conn = connect(":memory:", false).await;
    let Execution::Rows(mut stream) = conn
        .relational()
        .unwrap()
        .execute("SELECT range AS i FROM range(10000)")
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(stream.next_chunk(10).await.unwrap().unwrap().len(), 10);
    conn.ping().await.unwrap();
    conn.relational().unwrap().catalog("main").await.unwrap();
    let mut total = 10;
    while let Some(chunk) = stream.next_chunk(333).await.unwrap() {
        assert_eq!(chunk.cell(0, 0), Cell::Int(total));
        total += chunk.len() as i64;
    }
    assert_eq!(total, 10000);
}

#[tokio::test]
async fn runtime_errors_abort_transactions_until_rollback() {
    let conn = connect(":memory:", false).await;
    let rel = conn.relational().unwrap();
    rel.execute("CREATE TABLE tx_error(i INTEGER PRIMARY KEY)")
        .await
        .unwrap();
    rel.execute("INSERT INTO tx_error VALUES(1)").await.unwrap();
    rel.execute("BEGIN").await.unwrap();
    assert!(rel.execute("INSERT INTO tx_error VALUES(1)").await.is_err());
    assert!(rel.execute("SELECT 42").await.is_err());
    rel.execute("ROLLBACK").await.unwrap();
    assert_eq!(rows(&*conn, "SELECT 42").await[0][0], Value::Int(42));
}

#[tokio::test]
async fn timezone_values_keep_offsets_and_instant_precision() {
    let conn = connect(":memory:", false).await;
    let result=rows(&*conn,"SELECT TIMETZ '12:34:56.123456+05:30', TIMESTAMPTZ '2024-02-29 12:34:56.123456+05:30', TIMESTAMP '1969-12-31 23:59:59.999999'").await;
    assert_eq!(result[0][0], Value::Text("12:34:56.123456+05:30".into()));
    assert_eq!(
        result[0][1],
        Value::Text("2024-02-29 07:04:56.123456+00:00".into())
    );
    assert_eq!(
        result[0][2],
        Value::Text("1969-12-31 23:59:59.999999".into())
    );
}

#[tokio::test]
async fn nanosecond_time_preserves_precision_nulls_and_chunk_boundaries() {
    let conn = connect(":memory:", false).await;
    let Execution::Rows(mut stream) = conn
        .relational()
        .unwrap()
        .execute("SELECT i, CASE WHEN i % 3 = 0 THEN NULL ELSE '12:34:56.123456789'::TIME_NS END AS t FROM range(5000) r(i)")
        .await
        .unwrap() else {
        panic!("expected rows");
    };
    assert_eq!(stream.columns()[1].type_name, "TIME_NS");
    assert_eq!(
        stream.columns()[1].hint,
        sqlr_driver_sdk::DisplayHint::Temporal
    );
    let mut count = 0;
    while let Some(chunk) = stream.next_chunk(7).await.unwrap() {
        assert!(chunk.len() <= 7);
        for row in 0..chunk.len() {
            assert_eq!(chunk.cell(row, 0), Cell::Int(count));
            assert_eq!(
                chunk.cell(row, 1),
                if count % 3 == 0 {
                    Cell::Null
                } else {
                    Cell::Text("12:34:56.123456789")
                }
            );
            count += 1;
        }
    }
    assert_eq!(count, 5000);
    assert_eq!(rows(&*conn, "SELECT 42").await[0][0], Value::Int(42));
}

#[tokio::test]
async fn nanosecond_time_keeps_other_column_types_and_supports_returning() {
    let conn = connect(":memory:", false).await;
    let expressions = "true, 1::TINYINT, 2::SMALLINT, 3::INTEGER, 4::BIGINT, \
        5::UTINYINT, 6::USMALLINT, 7::UINTEGER, 18446744073709551615::UBIGINT, \
        170141183460469231731687303715884105727::HUGEINT, \
        340282366920938463463374607431768211455::UHUGEINT, \
        12.340::DECIMAL(10,3), 1.5::FLOAT, 2.5::DOUBLE, 'text', '\\x00\\xFF'::BLOB, \
        UUID '550e8400-e29b-41d4-a716-446655440000', \
        DATE '2026-10-27', TIME '12:34:56.123456', TIMETZ '12:34:56.123456+02', \
        TIMESTAMP 'infinity', TIMESTAMP '-infinity', \
        '2026-10-27 12:34:56.123456789'::TIMESTAMP_NS, \
        TIMESTAMPTZ '2026-10-27 12:34:56+02', INTERVAL '1 month 2 days 3 seconds', \
        [1, NULL, 3], {'a': 42}, NULL";
    let expected = rows(&*conn, &format!("SELECT {expressions}")).await;
    let mut actual = rows(
        &*conn,
        &format!("SELECT {expressions}, '00:00:00.000000001'::TIME_NS"),
    )
    .await;
    assert_eq!(
        actual[0].pop(),
        Some(Value::Text("00:00:00.000000001".into()))
    );
    assert_eq!(actual, expected);

    let rel = conn.relational().unwrap();
    rel.execute("CREATE TABLE times(t TIME_NS)").await.unwrap();
    assert_eq!(
        rows(
            &*conn,
            "INSERT INTO times VALUES ('23:59:59.999999999') RETURNING t"
        )
        .await,
        vec![vec![Value::Text("23:59:59.999999999".into())]]
    );
    assert_eq!(
        rows(&*conn, "SELECT count(*) FROM times").await[0][0],
        Value::Int(1)
    );
    assert!(
        rows(&*conn, "SELECT t FROM times WHERE false")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn bare_bound_values_keep_their_native_types() {
    let conn = connect(":memory:", false).await;
    let values = [
        Value::Bool(true),
        Value::Int(-42),
        Value::Text("bound text".into()),
        Value::Bytes(vec![0, 255]),
        Value::Null,
    ];
    let Execution::Rows(mut stream) = conn
        .relational()
        .unwrap()
        .execute_with("SELECT ?, ?, ?, ?, ?", &values)
        .await
        .unwrap()
    else {
        panic!()
    };
    let chunk = stream.next_chunk(1).await.unwrap().unwrap();
    assert_eq!(
        (0..values.len())
            .map(|i| Value::from_cell(chunk.cell(0, i)))
            .collect::<Vec<_>>(),
        values
    );
}
