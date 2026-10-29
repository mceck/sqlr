use sqlr_core::*;
use sqlr_driver_clickhouse::ClickHouseDriver;

#[test]
fn native_driver_declares_its_connection_and_capabilities() {
    let driver = ClickHouseDriver::new();
    let m = driver.manifest();
    assert_eq!(m.id, "clickhouse");
    assert!(m.capabilities.tabular_query && m.capabilities.server_cancel);
    assert!(!m.capabilities.transactions && !m.capabilities.editable_results);
    assert_eq!(m.default_port, Some(8123));
    assert!(m.fields.iter().any(|f| f.key == "password" && f.secret));
    assert!(
        m.fields
            .iter()
            .all(|f| !matches!(f.key.as_str(), "jar" | "java"))
    );
    assert!(driver.dialect().unwrap().database_is_schema());
}

fn config() -> ConnectionConfig {
    let (host, port) = sqlr_testkit::service("CLICKHOUSE");
    ConnectionConfig {
        id: "test".into(),
        name: "ClickHouse".into(),
        driver: "clickhouse".into(),
        group: None,
        color: None,
        profile: None,
        save_secrets: false,
        agent_access: AgentAccess::None,
        values: [
            ("host".into(), host),
            ("port".into(), port.to_string()),
            ("user".into(), "sqlr".into()),
        ]
        .into(),
    }
}
async fn connect() -> std::sync::Arc<dyn Connection> {
    ClickHouseDriver::new()
        .connect(
            &config(),
            &Secrets([("password".into(), "sqlr-test".into())].into()),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn arbitrary_queries_stream_typed_rows_and_empty_result_metadata() {
    sqlr_testkit::skip_container_tests!("CLICKHOUSE");
    let conn = connect().await;
    assert_eq!(conn.ping().await.unwrap().product, "ClickHouse");
    let Execution::Rows(mut stream) = conn
        .relational()
        .unwrap()
        .execute("SELECT toInt64(number) AS n, 'question?; è' AS text FROM numbers(5);")
        .await
        .unwrap()
    else {
        panic!("expected rows")
    };
    assert_eq!(stream.columns()[0].kind, ColumnKind::Int);
    let chunk = stream.next_chunk(2).await.unwrap().unwrap();
    assert_eq!(chunk.len(), 2);
    assert_eq!(chunk.cell(1, 0), Cell::Int(1));
    assert_eq!(chunk.cell(1, 1), Cell::Text("question?; è"));
    assert_eq!(stream.next_chunk(2).await.unwrap().unwrap().len(), 2);
    assert_eq!(
        stream.next_chunk(2).await.unwrap().unwrap().cell(0, 0),
        Cell::Int(4)
    );
    assert!(stream.next_chunk(2).await.unwrap().is_none());
    stream.close().await.unwrap();
    let Execution::Rows(mut empty) = conn
        .relational()
        .unwrap()
        .execute("SELECT toInt64(1) AS n WHERE 0")
        .await
        .unwrap()
    else {
        panic!("expected rows")
    };
    assert_eq!(empty.columns()[0].name, "n");
    assert!(empty.next_chunk(10).await.unwrap().is_none());
}

#[tokio::test]
async fn exact_numbers_nullable_and_nested_types_keep_their_values() {
    sqlr_testkit::skip_container_tests!("CLICKHOUSE");
    let conn = connect().await;
    let Execution::Rows(mut stream) = conn.relational().unwrap().execute("SELECT toUInt64('18446744073709551615') AS u, toDecimal128('12345678901234567890.123456789', 9) AS d, CAST(NULL AS Nullable(Int64)) AS nil, true AS b, toDate('2026-10-29') AS day, [1,2] AS a, toLowCardinality('è') AS text, toFloat64('inf') AS f").await.unwrap() else { panic!("rows") };
    let chunk = stream.next_chunk(10).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Text("18446744073709551615"));
    assert_eq!(
        chunk.cell(0, 1),
        Cell::Text("12345678901234567890.123456789")
    );
    assert_eq!(chunk.cell(0, 2), Cell::Null);
    assert_eq!(chunk.cell(0, 3), Cell::Bool(true));
    assert_eq!(chunk.cell(0, 4), Cell::Text("2026-10-29"));
    assert_eq!(chunk.cell(0, 5), Cell::Text("[1,2]"));
    assert_eq!(chunk.cell(0, 6), Cell::Text("è"));
    assert_eq!(chunk.cell(0, 7), Cell::Float(f64::INFINITY));
    assert_eq!(stream.columns()[1].hint, DisplayHint::Decimal);
}

#[tokio::test]
async fn bound_parameters_preserve_literals_comments_and_injection_text() {
    sqlr_testkit::skip_container_tests!("CLICKHOUSE");
    let conn = connect().await;
    let text = "x'\\?; DROP DATABASE default; -- è";
    let Execution::Rows(mut stream) = conn
        .relational()
        .unwrap()
        .execute_with(
            "SELECT ? AS text, '?' AS literal, ? AS n /* ? */",
            &[Value::Text(text.into()), Value::Int(42)],
        )
        .await
        .unwrap()
    else {
        panic!("rows")
    };
    let chunk = stream.next_chunk(1).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Text(text));
    assert_eq!(chunk.cell(0, 1), Cell::Text("?"));
    assert_eq!(chunk.cell(0, 2), Cell::Int(42));
    stream.close().await.unwrap();
    assert!(
        conn.relational()
            .unwrap()
            .execute_with("SELECT ?", &[])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn catalog_browses_tables_views_and_describes_non_unique_sorting_keys() {
    sqlr_testkit::skip_container_tests!("CLICKHOUSE");
    let conn = connect().await;
    let db = sqlr_testkit::unique_name("sqlr");
    let rel = conn.relational().unwrap();
    rel.execute(&format!("CREATE DATABASE `{db}`"))
        .await
        .unwrap();
    rel.execute(&format!("CREATE TABLE `{db}`.`events` (id Int64, label Nullable(String) DEFAULT NULL, derived Int64 MATERIALIZED id + 1) ENGINE = MergeTree ORDER BY id")).await.unwrap();
    rel.execute(&format!(
        "CREATE VIEW `{db}`.`v` AS SELECT id FROM `{db}`.`events`"
    ))
    .await
    .unwrap();
    let nodes = conn.browse(&NodePath::root()).await.unwrap();
    assert!(nodes.iter().any(|n| n.name == db));
    let nodes = conn.browse(&NodePath::root().child(&db)).await.unwrap();
    assert!(
        nodes
            .iter()
            .any(|n| n.name == "v" && n.kind == NodeKind::View)
    );
    let table = TableRef {
        database: None,
        schema: Some(db.clone()),
        name: "events".into(),
    };
    let info = rel.describe(&table).await.unwrap();
    assert!(
        info.primary_key.is_empty(),
        "ClickHouse sorting keys do not identify unique rows"
    );
    assert!(info.column("label").unwrap().nullable);
    assert!(info.column("derived").unwrap().generated);
    assert_eq!(
        conn.browse(&NodePath::root().child(&db).child("events"))
            .await
            .unwrap()
            .len(),
        3
    );
    let cat = rel.catalog(&db).await.unwrap();
    assert_eq!(cat.relations.len(), 2);
    assert_eq!(cat.relations[0].columns.len(), 3);
    assert!(
        rel.catalog("x' OR 1=1 --")
            .await
            .unwrap()
            .relations
            .is_empty()
    );
    rel.execute(&format!("DROP DATABASE `{db}`")).await.unwrap();
}

#[tokio::test]
async fn writes_report_rows_and_read_only_rejects_mutations() {
    sqlr_testkit::skip_container_tests!("CLICKHOUSE");
    let conn = connect().await;
    let rel = conn.relational().unwrap();
    let table = sqlr_testkit::unique_name("events");
    rel.execute(&format!(
        "CREATE TABLE `{table}` (id Int64) ENGINE = Memory"
    ))
    .await
    .unwrap();
    let written = rel
        .execute(&format!("INSERT INTO `{table}` VALUES (1),(2)"))
        .await
        .unwrap();
    assert!(matches!(written, Execution::Affected(2)));
    rel.execute(&rel.dialect().read_only_sql(true).unwrap())
        .await
        .unwrap();
    let err = rel
        .execute(&format!("INSERT INTO `{table}` VALUES (3)"))
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Permission);
    rel.execute(&rel.dialect().read_only_sql(false).unwrap())
        .await
        .unwrap();
    rel.execute(&format!("DROP TABLE `{table}`")).await.unwrap();
    assert_eq!(
        rel.execute("BEGIN").await.err().unwrap().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        rel.execute("USE system").await.err().unwrap().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        rel.execute("SELEC 1").await.err().unwrap().kind,
        ErrorKind::Syntax
    );
}

#[tokio::test]
async fn cancel_stops_the_server_query_and_connection_can_be_reused() {
    sqlr_testkit::skip_container_tests!("CLICKHOUSE");
    let conn = connect().await;
    let cancel = conn.cancel_handle();
    let query_conn = conn.clone();
    let task = tokio::spawn(async move {
        match query_conn
            .relational()
            .unwrap()
            .execute("SELECT sleepEachRow(0.00001) FROM numbers(1000000000)")
            .await
        {
            Err(err) => Err(err),
            Ok(Execution::Rows(mut stream)) => loop {
                match stream.next_chunk(100).await {
                    Ok(Some(_)) => (),
                    Ok(None) => break Ok(()),
                    Err(err) => break Err(err),
                }
            },
            _ => panic!("expected rows"),
        }
    });
    // Wait through a separate public Driver connection until the server has the query.
    let observer = connect().await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let Execution::Rows(mut stream) = observer
                .relational()
                .unwrap()
                .execute(
                    "SELECT count() FROM system.processes WHERE query LIKE 'SELECT sleepEachRow%'",
                )
                .await
                .unwrap()
            else {
                panic!("rows")
            };
            let chunk = stream.next_chunk(1).await.unwrap().unwrap();
            let running = chunk.cell(0, 0) != Cell::Text("0");
            stream.close().await.unwrap();
            if running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    cancel.cancel().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(conn.ping().await.unwrap().product, "ClickHouse");
    cancel.cancel().await.unwrap();
}

#[tokio::test]
async fn authentication_errors_and_bad_configuration_are_reported_without_passwords() {
    sqlr_testkit::skip_container_tests!("CLICKHOUSE");
    let driver = ClickHouseDriver::new();
    let error = driver
        .connect(
            &config(),
            &Secrets([("password".into(), "wrong-secret".into())].into()),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Auth);
    assert!(!error.message.contains("wrong-secret"));
    let mut cfg = config();
    cfg.values
        .insert("host".into(), "user:password@localhost/path".into());
    assert_eq!(
        driver
            .connect(&cfg, &Secrets::default())
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Connection
    );
}

#[tokio::test]
async fn native_driver_passes_the_shared_conformance_suite() {
    sqlr_testkit::skip_container_tests!("CLICKHOUSE");
    let admin = connect().await;
    let database = sqlr_testkit::unique_name("conformance");
    admin
        .relational()
        .unwrap()
        .execute(&format!("CREATE DATABASE `{database}`"))
        .await
        .unwrap();
    let db = database.clone();
    let mut target = sqlr_driver_tests::Target::new("ClickHouse", Box::new(move || {
        let db = db.clone();
        Box::pin(async move {
            let conn = connect().await;
            conn.connect_database(&db).await
        })
    })).with_setup([
        "CREATE TABLE sqlr_conformance (n Int64, t String) ENGINE = Memory",
        "INSERT INTO sqlr_conformance SELECT toInt64(number), concat('row ', toString(number)) FROM numbers(1,25)",
    ]);
    target.capabilities = ClickHouseDriver::new().manifest().capabilities;
    target.affected_sql = Some(
        "INSERT INTO sqlr_conformance SELECT toInt64(number), 'extra' FROM numbers(26,3)".into(),
    );
    target.slow_sql = Some("SELECT sleepEachRow(0.00001) FROM numbers(1000000000)".into());
    target.type_cases = vec![sqlr_driver_tests::TypeCase {
        name: "UInt64 exact".into(),
        sql: "SELECT toUInt64('18446744073709551615')".into(),
        expect: Value::Text("18446744073709551615".into()),
    }];
    let report = sqlr_driver_tests::relational_suite(&target).await;
    admin
        .relational()
        .unwrap()
        .execute(&format!("DROP DATABASE `{database}`"))
        .await
        .unwrap();
    assert!(report.failed.is_empty(), "{report}");
}

#[tokio::test]
async fn null_parameters_and_quoted_identifiers_round_trip() {
    sqlr_testkit::skip_container_tests!("CLICKHOUSE");
    let conn = connect().await;
    let rel = conn.relational().unwrap();
    let ident = "a`b\\c";
    let sql = format!(
        "SELECT ? AS {}, ? AS flag, ? AS f, 'ᴺᵁᴸᴸ' AS text",
        rel.dialect().quote_ident(ident)
    );
    let Execution::Rows(mut stream) = rel
        .execute_with(&sql, &[Value::Null, Value::Bool(true), Value::Float(1.25)])
        .await
        .unwrap()
    else {
        panic!("rows")
    };
    assert_eq!(stream.columns()[0].name, ident);
    let chunk = stream.next_chunk(10).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Null);
    assert_eq!(chunk.cell(0, 1), Cell::Bool(true));
    assert_eq!(chunk.cell(0, 2), Cell::Float(1.25));
    assert_eq!(chunk.cell(0, 3), Cell::Text("ᴺᵁᴸᴸ"));
}

#[tokio::test]
async fn dropping_an_unfinished_result_stops_its_server_query() {
    sqlr_testkit::skip_container_tests!("CLICKHOUSE");
    let conn = connect().await;
    let tag = sqlr_testkit::unique_name("dropped_result");
    let sql = format!("SELECT number FROM numbers(1000000000) /* {tag} */");
    let Execution::Rows(mut stream) = conn.relational().unwrap().execute(&sql).await.unwrap()
    else {
        panic!("rows")
    };
    assert_eq!(stream.next_chunk(1).await.unwrap().unwrap().len(), 1);
    drop(stream);
    let observer = connect().await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let Execution::Rows(mut stream) = observer.relational().unwrap().execute_with("SELECT count() FROM system.processes WHERE query_id != currentQueryID() AND query LIKE ?", &[Value::Text(format!("%{tag}%"))]).await.unwrap() else { panic!("rows") };
            let chunk = stream.next_chunk(10).await.unwrap().unwrap();
            if chunk.cell(0, 0) == Cell::Text("0") { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    assert_eq!(conn.ping().await.unwrap().product, "ClickHouse");
}
