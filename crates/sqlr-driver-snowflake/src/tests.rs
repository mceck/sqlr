use super::*;
use serde_json::{Value as Json, json};
use sqlr_core::{Cell, ConnectionConfig, Driver, ErrorKind, Execution, NodePath, Secrets, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    time::Duration,
};

struct Fixture {
    endpoint: String,
    requests: Arc<Mutex<Vec<String>>>,
}
impl Fixture {
    fn new(responses: Vec<(u16, Json)>) -> Self {
        Self::raw(
            responses
                .into_iter()
                .map(|(status, body)| (status, String::new(), body.to_string().into_bytes()))
                .collect(),
        )
    }
    fn raw(responses: Vec<(u16, String, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        std::thread::spawn(move || {
            for (status, headers, body) in responses {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0; 4096];
                loop {
                    let n = socket.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let length: usize = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|s| s.parse().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(request).unwrap());
                write!(socket, "HTTP/1.1 {status} fixture\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                socket.write_all(&body).unwrap();
            }
        });
        Self { endpoint, requests }
    }
    async fn connection(&self) -> Arc<dyn sqlr_core::Connection> {
        SnowflakeDriver::for_test(&self.endpoint)
            .connect(
                &config(),
                &Secrets([("token".into(), "super-secret-token".into())].into()),
            )
            .await
            .unwrap()
    }
}
fn config() -> ConnectionConfig {
    ConnectionConfig {
        id: "test".into(),
        name: "test".into(),
        driver: "snowflake".into(),
        group: None,
        color: None,
        values: [
            ("host".into(), "example.snowflakecomputing.com".into()),
            ("database".into(), "DB".into()),
            ("schema".into(), "PUBLIC".into()),
            ("warehouse".into(), "WH".into()),
        ]
        .into(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    }
}
fn result(types: &[(&str, &str)], data: Json, counts: &[usize]) -> Json {
    json!({"statementHandle":"q-123", "statementTypeId":4096, "code":"090001", "sqlState":"00000", "resultSetMetaData":{"numRows":counts.iter().sum::<usize>(),"format":"jsonv2","rowType":types.iter().map(|(name,ty)|json!({"name":name,"type":ty})).collect::<Vec<_>>(),"partitionInfo":counts.iter().map(|n|json!({"rowCount":n})).collect::<Vec<_>>()},"data":data})
}
fn err(execution: Result<Execution, sqlr_core::DriverError>) -> sqlr_core::DriverError {
    match execution {
        Err(e) => e,
        Ok(_) => panic!("expected error"),
    }
}

#[tokio::test]
async fn polls_and_fetches_partitions_lazily_with_exact_chunks_and_bindings() {
    let fixture = Fixture::new(vec![
        (202, json!({"statementHandle":"q-123"})),
        (
            200,
            result(
                &[("N", "fixed"), ("D", "date"), ("B", "binary")],
                json!([
                    [
                        "12345678901234567890123456789012345678",
                        "2026-10-25",
                        "00ff"
                    ],
                    ["1.2300", null, ""]
                ]),
                &[2, 1],
            ),
        ),
        (200, json!({"data":[["4","2026-10-26","ab"]]})),
    ]);
    let conn = fixture.connection().await;
    let Execution::Rows(mut stream) = conn
        .relational()
        .unwrap()
        .execute_with(
            "select ?, ?, ?",
            &[Value::Int(7), Value::Text("O'Reilly".into()), Value::Null],
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    let chunk = stream.next_chunk(1).await.unwrap().unwrap();
    assert_eq!(
        chunk.cell(0, 0),
        Cell::Text("12345678901234567890123456789012345678")
    );
    assert_eq!(chunk.cell(0, 2), Cell::Bytes(&[0, 255]));
    assert_eq!(stream.columns()[1].hint, sqlr_core::DisplayHint::Temporal);
    assert_eq!(
        stream.next_chunk(1).await.unwrap().unwrap().cell(0, 0),
        Cell::Text("1.2300")
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    assert_eq!(stream.next_chunk(1).await.unwrap().unwrap().len(), 1);
    assert!(stream.next_chunk(1).await.unwrap().is_none());
    let requests = fixture.requests.lock().unwrap();
    let body: Json = serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["bindings"]["1"], json!({"type":"FIXED","value":"7"}));
    assert_eq!(body["bindings"]["2"]["value"], "O'Reilly");
    assert!(body["bindings"]["3"]["value"].is_null());
    assert_eq!(body["parameters"]["date_output_format"], "YYYY-MM-DD");
    assert!(requests[2].starts_with("GET /api/v2/statements/q-123?partition=1"));
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("x-snowflake-authorization-token-type: oauth")
    );
}

#[tokio::test]
async fn dml_uses_statement_type_and_sums_merge_counts_while_select_count_stays_rows() {
    let mut merge = result(
        &[
            ("inserted", "fixed"),
            ("updated", "fixed"),
            ("deleted", "fixed"),
        ],
        json!([["2", "3", "4"]]),
        &[1],
    );
    merge["statementTypeId"] = json!(13312);
    let fixture = Fixture::new(vec![
        (200, merge),
        (200, result(&[("COUNT(*)", "fixed")], json!([["99"]]), &[1])),
    ]);
    let conn = fixture.connection().await;
    assert!(matches!(
        conn.relational()
            .unwrap()
            .execute("merge into x using y on true when matched then delete")
            .await
            .unwrap(),
        Execution::Affected(9)
    ));
    assert!(matches!(
        conn.relational()
            .unwrap()
            .execute("select count(*) from x")
            .await
            .unwrap(),
        Execution::Rows(_)
    ));
}

#[tokio::test]
async fn maps_errors_without_server_message_sql_or_token() {
    for (status, state, kind) in [
        (401, "28000", ErrorKind::Auth),
        (422, "57014", ErrorKind::Cancelled),
        (403, "42501", ErrorKind::Permission),
    ] {
        let fixture = Fixture::new(vec![(
            status,
            json!({"sqlState":state,"message":"super-secret-token select sensitive", "code":"123"}),
        )]);
        let conn = fixture.connection().await;
        let error = err(conn.relational().unwrap().execute("select sensitive").await);
        assert_eq!(error.kind, kind);
        assert!(!format!("{error:?}").contains("super-secret-token"));
        assert!(!error.message.contains("sensitive"));
    }
}

#[tokio::test]
async fn statement_errors_keep_the_server_message_without_the_token() {
    for (state, kind) in [("42000", ErrorKind::Syntax), ("22018", ErrorKind::Other)] {
        let fixture = Fixture::new(vec![(
            422,
            json!({"sqlState":state, "code":"001003", "statementHandle":"q-1",
                "message":"SQL compilation error:\nsyntax error line 1 at position 7 unexpected 'FORM'. super-secret-token"}),
        )]);
        let conn = fixture.connection().await;
        let error = err(conn.relational().unwrap().execute("select * form t").await);
        assert_eq!(error.kind, kind);
        assert!(
            error
                .message
                .contains("line 1 at position 7 unexpected 'FORM'"),
            "{}",
            error.message
        );
        assert!(!format!("{error:?}").contains("super-secret-token"));
    }
}

#[tokio::test]
async fn validates_rows_and_partition_lengths_instead_of_losing_data() {
    for response in [
        result(&[("A", "text")], json!([["a", "b"]]), &[1]),
        result(&[("A", "text")], json!([]), &[1]),
        json!({"data":[]}),
    ] {
        let fixture = Fixture::new(vec![(200, response)]);
        let conn = fixture.connection().await;
        assert_eq!(
            err(conn.relational().unwrap().execute("select 1").await).kind,
            ErrorKind::Protocol
        );
    }
}

#[tokio::test]
async fn session_controls_are_rejected_even_after_comments() {
    let fixture = Fixture::new(vec![]);
    let conn = fixture.connection().await;
    for sql in [
        "USE DATABASE other",
        "-- comment\nBEGIN",
        "/* c */ SET x=1",
        "COMMIT",
        "ROLLBACK",
        "ALTER SESSION SET timezone='UTC'",
        "START TRANSACTION",
        "UNSET x",
        "SAVEPOINT s",
    ] {
        assert_eq!(
            err(conn.relational().unwrap().execute(sql).await).kind,
            ErrorKind::Unsupported,
            "{sql}"
        );
    }
}

#[tokio::test]
async fn cancels_only_current_handle_and_never_reuses_it() {
    let fixture = Fixture::new(vec![
        (200, result(&[("A", "text")], json!([["x"]]), &[1])),
        (200, json!({"code":"000604","sqlState":"57014"})),
        (200, result(&[("A", "text")], json!([["y"]]), &[1])),
    ]);
    let conn = fixture.connection().await;
    let Execution::Rows(mut stream) = conn
        .relational()
        .unwrap()
        .execute("select 1")
        .await
        .unwrap()
    else {
        panic!()
    };
    conn.cancel_handle().cancel().await.unwrap();
    assert_eq!(
        stream.next_chunk(1).await.unwrap_err().kind,
        ErrorKind::Cancelled
    );
    stream.close().await.unwrap();
    conn.cancel_handle().cancel().await.unwrap();
    let Execution::Rows(mut next) = conn
        .relational()
        .unwrap()
        .execute("select 2")
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        next.next_chunk(1).await.unwrap().unwrap().cell(0, 0),
        Cell::Text("y")
    );
    assert!(
        fixture.requests.lock().unwrap()[1].starts_with("POST /api/v2/statements/q-123/cancel")
    );
}

#[tokio::test]
async fn catalogs_parameterize_unusual_names() {
    let fixture = Fixture::new(vec![
        (
            200,
            result(&[("SCHEMA_NAME", "text")], json!([["a'b"]]), &[1]),
        ),
        (
            200,
            result(
                &[("TABLE_NAME", "text"), ("TABLE_TYPE", "text")],
                json!([["T", "BASE TABLE"], ["V", "VIEW"]]),
                &[2],
            ),
        ),
        (
            200,
            result(
                &[
                    ("TABLE_NAME", "text"),
                    ("COLUMN_NAME", "text"),
                    ("DATA_TYPE", "text"),
                ],
                json!([["T", "C", "NUMBER"], ["V", "C", "NUMBER"]]),
                &[2],
            ),
        ),
    ]);
    let conn = fixture.connection().await;
    assert_eq!(conn.browse(&NodePath::root()).await.unwrap()[0].name, "a'b");
    let catalog = conn.relational().unwrap().catalog("a'b").await.unwrap();
    assert_eq!(catalog.relations.len(), 2);
    assert_eq!(catalog.relations[0].columns[0].name, "C");
    for request in fixture.requests.lock().unwrap().iter().skip(1) {
        let body: Json = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert!(!body["statement"].as_str().unwrap().contains("a'b"));
        assert_eq!(body["bindings"]["1"]["value"], "a'b");
    }
}

#[tokio::test]
async fn https_and_secret_requirements_and_manifest() {
    let driver = SnowflakeDriver::new();
    assert!(!driver.manifest().capabilities.transactions);
    assert!(!driver.manifest().capabilities.editable_results);
    assert!(
        driver
            .manifest()
            .fields
            .iter()
            .find(|f| f.key == "token")
            .unwrap()
            .secret
    );
    assert!(
        driver
            .connect(&config(), &Secrets::default())
            .await
            .is_err()
    );
    let mut cfg = config();
    cfg.values
        .insert("host".into(), "http://example.com".into());
    assert!(
        driver
            .connect(&cfg, &Secrets([("token".into(), "secret".into())].into()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelled_success_response_and_network_failure_are_classified() {
    let fixture = Fixture::new(vec![(200, json!({"sqlState":"57014","code":"000604"}))]);
    let conn = fixture.connection().await;
    assert_eq!(
        err(conn.relational().unwrap().execute("select 1").await).kind,
        ErrorKind::Cancelled
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let conn = SnowflakeDriver::for_test(&endpoint)
        .connect(
            &config(),
            &Secrets([("token".into(), "secret".into())].into()),
        )
        .await
        .unwrap();
    let error = err(conn.relational().unwrap().execute("select sensitive").await);
    assert_eq!(error.kind, ErrorKind::Connection);
    assert!(!error.message.contains(&endpoint));
}

#[tokio::test]
async fn incomplete_initial_response_refetches_partition_zero_lazily() {
    let mut response = result(&[("A", "text")], json!([]), &[2]);
    response["code"] = json!("391908");
    let fixture = Fixture::new(vec![(200, response), (200, json!({"data":[["a"],["b"]]}))]);
    let conn = fixture.connection().await;
    let Execution::Rows(mut stream) = conn
        .relational()
        .unwrap()
        .execute("select 1")
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    assert_eq!(
        stream.next_chunk(1).await.unwrap().unwrap().cell(0, 0),
        Cell::Text("a")
    );
    assert!(
        fixture.requests.lock().unwrap()[1].starts_with("GET /api/v2/statements/q-123?partition=0")
    );
}

#[tokio::test]
async fn describes_columns_and_preserves_null_defaults() {
    let fixture = Fixture::new(vec![(
        200,
        result(
            &[
                ("NAME", "text"),
                ("TYPE", "text"),
                ("NULLABLE", "text"),
                ("DEFAULT", "text"),
                ("IDENTITY", "text"),
            ],
            json!([
                ["id", "NUMBER", "NO", null, "YES"],
                ["body", "VARCHAR", "YES", "'hello'", "NO"]
            ]),
            &[2],
        ),
    )]);
    let conn = fixture.connection().await;
    let table = TableRef {
        database: None,
        schema: Some("s'c".into()),
        name: "t\"a".into(),
    };
    let info = conn.relational().unwrap().describe(&table).await.unwrap();
    assert!(info.columns[0].automatic);
    assert!(!info.columns[0].nullable);
    assert_eq!(info.columns[0].default, None);
    assert_eq!(info.columns[1].default, Some("'hello'".into()));
}

#[tokio::test]
async fn older_stream_drop_cannot_clear_new_query_cancellation() {
    let fixture = Fixture::new(vec![
        (200, result(&[("A", "text")], json!([["a"]]), &[1])),
        (200, result(&[("A", "text")], json!([["b"]]), &[1])),
        (200, json!({"sqlState":"57014","code":"000604"})),
    ]);
    let conn = fixture.connection().await;
    let first = conn
        .relational()
        .unwrap()
        .execute("select 1")
        .await
        .unwrap();
    let Execution::Rows(mut second) = conn
        .relational()
        .unwrap()
        .execute("select 2")
        .await
        .unwrap()
    else {
        panic!()
    };
    drop(first);
    conn.cancel_handle().cancel().await.unwrap();
    assert_eq!(
        second.next_chunk(1).await.unwrap_err().kind,
        ErrorKind::Cancelled
    );
}

#[tokio::test]
async fn polling_is_cancellable_without_holding_the_active_lock() {
    let fixture = Fixture::new(vec![
        (202, json!({"statementHandle":"q-123"})),
        (200, json!({"sqlState":"57014","code":"000604"})),
    ]);
    let conn = fixture.connection().await;
    let executing = conn.clone();
    let task =
        tokio::spawn(async move { executing.relational().unwrap().execute("select 1").await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if fixture.requests.lock().unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    conn.cancel_handle().cancel().await.unwrap();
    assert_eq!(err(task.await.unwrap()).kind, ErrorKind::Cancelled);
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn update_does_not_double_count_multi_joined_rows() {
    let mut update = result(
        &[
            ("number of rows updated", "fixed"),
            ("number of multi-joined rows updated", "fixed"),
        ],
        json!([["4", "2"]]),
        &[1],
    );
    update["statementTypeId"] = json!(0x3200);
    let fixture = Fixture::new(vec![(200, update)]);
    let conn = fixture.connection().await;
    assert!(matches!(
        conn.relational()
            .unwrap()
            .execute("UPDATE t SET x=1 FROM s")
            .await
            .unwrap(),
        Execution::Affected(4)
    ));
}

#[tokio::test]
async fn compressed_partitions_decode_natively() {
    let compressed = vec![
        31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 171, 86, 74, 73, 44, 73, 84, 178, 138, 142, 86, 74, 82,
        138, 141, 173, 5, 0, 198, 19, 6, 134, 16, 0, 0, 0,
    ];
    let fixture = Fixture::raw(vec![
        (
            200,
            String::new(),
            result(&[("A", "text")], json!([["a"]]), &[1, 1])
                .to_string()
                .into_bytes(),
        ),
        (200, "Content-Encoding: gzip\r\n".into(), compressed),
    ]);
    let conn = fixture.connection().await;
    let Execution::Rows(mut stream) = conn
        .relational()
        .unwrap()
        .execute("select a")
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        stream.next_chunk(2).await.unwrap().unwrap().cell(1, 0),
        Cell::Text("b")
    );
}

#[tokio::test]
async fn redirects_do_not_forward_credentials() {
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    destination.set_nonblocking(true).unwrap();
    let fixture = Fixture::raw(vec![(
        302,
        format!(
            "Location: http://{}/capture\r\n",
            destination.local_addr().unwrap()
        ),
        b"{}".to_vec(),
    )]);
    let conn = fixture.connection().await;
    assert!(
        conn.relational()
            .unwrap()
            .execute("select 1")
            .await
            .is_err()
    );
    assert_eq!(
        destination.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[tokio::test]
async fn auth_variants_and_server_urls_never_override_the_request_destination() {
    for auth in ["PROGRAMMATIC_ACCESS_TOKEN", "KEYPAIR_JWT"] {
        let fixture = Fixture::new(vec![
            (
                429,
                json!({"statementHandle":"q-123","statementStatusUrl":"http://attacker.invalid/steal"}),
            ),
            (200, result(&[("A", "text")], json!([["a"]]), &[1])),
        ]);
        let mut cfg = config();
        cfg.values.insert("auth_type".into(), auth.into());
        let conn = SnowflakeDriver::for_test(&fixture.endpoint)
            .connect(&cfg, &Secrets([("token".into(), "secret".into())].into()))
            .await
            .unwrap();
        assert!(conn.relational().unwrap().execute("select 1").await.is_ok());
        let requests = fixture.requests.lock().unwrap();
        assert!(requests[0].to_ascii_lowercase().contains(&format!(
            "x-snowflake-authorization-token-type: {}",
            auth.to_ascii_lowercase()
        )));
        assert!(requests[1].starts_with("GET /api/v2/statements/q-123"));
    }
}

/// Explicit opt-in: no account is contacted unless both environment variables exist.
#[tokio::test]
async fn live_smoke_when_credentials_are_supplied() {
    let (Ok(host), Ok(token)) = (
        std::env::var("SQLR_TEST_SNOWFLAKE_HOST"),
        std::env::var("SQLR_TEST_SNOWFLAKE_TOKEN"),
    ) else {
        return;
    };
    let mut cfg = config();
    cfg.values.clear();
    cfg.values.insert("host".into(), host);
    for key in ["database", "schema", "warehouse", "role", "auth_type"] {
        if let Ok(value) =
            std::env::var(format!("SQLR_TEST_SNOWFLAKE_{}", key.to_ascii_uppercase()))
        {
            cfg.values.insert(key.into(), value);
        }
    }
    let conn = SnowflakeDriver::new()
        .connect(&cfg, &Secrets([("token".into(), token)].into()))
        .await
        .unwrap();
    assert_eq!(conn.ping().await.unwrap().product, "Snowflake");
    let Execution::Rows(mut rows) = conn
        .relational()
        .unwrap()
        .execute_with(
            "SELECT ?::NUMBER(38,0), ?::VARCHAR",
            &[Value::Int(42), Value::Text("sqlr smoke".into())],
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    let chunk = rows.next_chunk(1).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Text("42"));
    assert_eq!(chunk.cell(0, 1), Cell::Text("sqlr smoke"));
    rows.close().await.unwrap();
    let schemas = conn.browse(&NodePath::root()).await.unwrap();
    if let Some(schema) = schemas.first() {
        conn.relational()
            .unwrap()
            .catalog(&schema.name)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn empty_results_are_rows_with_columns_and_close_is_idempotent() {
    let fixture = Fixture::new(vec![(200, result(&[("N", "fixed")], json!([]), &[0]))]);
    let conn = fixture.connection().await;
    let Execution::Rows(mut rows) = conn
        .relational()
        .unwrap()
        .execute("select 1 where false")
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(rows.columns()[0].name, "N");
    assert!(rows.next_chunk(10).await.unwrap().is_none());
    rows.close().await.unwrap();
    rows.close().await.unwrap();
    conn.cancel_handle().cancel().await.unwrap();
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_server_handles_are_rejected_before_any_followup() {
    let fixture = Fixture::new(vec![(
        202,
        json!({"statementHandle":"../steal?token=secret"}),
    )]);
    let conn = fixture.connection().await;
    let error = err(conn.relational().unwrap().execute("select 1").await);
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(!error.message.contains("secret"));
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn configured_schema_drives_unqualified_autocomplete() {
    let fixture = Fixture::new(vec![]);
    let mut cfg = config();
    cfg.values.insert("schema".into(), "Custom schema".into());
    let driver = SnowflakeDriver::for_test(&fixture.endpoint);
    assert_eq!(
        driver.dialect_for(&cfg).unwrap().default_schemas(),
        vec!["Custom schema"]
    );
    let conn = driver
        .connect(&cfg, &Secrets([("token".into(), "secret".into())].into()))
        .await
        .unwrap();
    assert_eq!(
        conn.relational().unwrap().dialect().default_schemas(),
        vec!["Custom schema"]
    );
}

#[tokio::test]
async fn throttled_submission_without_handle_is_not_retried_or_called_malformed() {
    let fixture = Fixture::new(vec![(429, json!({"code":"429","message":"rate limited"}))]);
    let conn = fixture.connection().await;
    assert_eq!(
        err(conn
            .relational()
            .unwrap()
            .execute("INSERT INTO t VALUES(1)")
            .await)
        .kind,
        ErrorKind::Timeout
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn unavailable_service_with_html_response_is_a_connection_error() {
    let fixture = Fixture::raw(vec![(
        503,
        String::new(),
        b"<html>unavailable</html>".to_vec(),
    )]);
    let conn = fixture.connection().await;
    assert_eq!(
        err(conn.relational().unwrap().execute("select 1").await).kind,
        ErrorKind::Connection
    );
}

#[tokio::test]
async fn partition_throttling_retries_only_the_safe_get() {
    let fixture = Fixture::new(vec![
        (200, result(&[("A", "text")], json!([["a"]]), &[1, 1])),
        (429, json!({"statementHandle":"q-123"})),
        (200, json!({"data":[["b"]]})),
    ]);
    let conn = fixture.connection().await;
    let Execution::Rows(mut rows) = conn
        .relational()
        .unwrap()
        .execute("select a")
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(rows.next_chunk(2).await.unwrap().unwrap().len(), 2);
    let requests = fixture.requests.lock().unwrap();
    assert!(requests[1].starts_with("GET /api/v2/statements/q-123?partition=1"));
    assert!(requests[2].starts_with("GET /api/v2/statements/q-123?partition=1"));
}
