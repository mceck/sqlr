mod support;

use std::sync::Arc;
use std::time::Duration;

use sqlr_core::{
    Cell, ColumnKind, ColumnOrigin, Connection, DisplayHint, Driver, ErrorKind, Execution, Node,
    NodeKind, NodePath, RowStream, TableRef, Value,
};
use sqlr_driver_postgres::PostgresDriver;
use support::pg;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;

async fn connect() -> (ContainerAsync<Postgres>, Arc<dyn Connection>) {
    let (container, cfg, sec) = pg().await;
    let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    (container, conn)
}

async fn exec(conn: &Arc<dyn Connection>, sql: &str) -> Execution {
    conn.relational()
        .unwrap()
        .execute(sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"))
}

async fn rows(conn: &Arc<dyn Connection>, sql: &str) -> Box<dyn RowStream> {
    match exec(conn, sql).await {
        Execution::Rows(s) => s,
        Execution::Affected(n) => panic!("{sql}: expected rows, got Affected({n})"),
    }
}

async fn affected(conn: &Arc<dyn Connection>, sql: &str) -> u64 {
    match exec(conn, sql).await {
        Execution::Affected(n) => n,
        Execution::Rows(_) => panic!("{sql}: expected Affected, got rows"),
    }
}

#[tokio::test]
async fn streams_in_pages() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let mut s = rows(&conn, "SELECT g AS n FROM generate_series(1, 1234) g").await;
    assert_eq!(s.columns()[0].name, "n");
    let mut lens = Vec::new();
    let mut first = None;
    while let Some(chunk) = s.next_chunk(500).await.unwrap() {
        if first.is_none() {
            first = Some(chunk.cell(0, 0) == Cell::Int(1));
        }
        lens.push(chunk.len());
    }
    assert_eq!(lens, [500, 500, 234]);
    assert_eq!(first, Some(true));
    assert!(s.next_chunk(500).await.unwrap().is_none());
    s.close().await.unwrap();
    s.close().await.unwrap();
}

#[tokio::test]
async fn columns_typed() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let mut s = rows(
        &conn,
        r#"SELECT true AS b, 1::int8 AS i, 1.5::float8 AS f,
                  12345678901234567890.5::numeric AS d, now() AS t,
                  '{"a":1}'::jsonb AS j, 'x'::text AS s"#,
    )
    .await;
    let cols = s.columns();
    let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["b", "i", "f", "d", "t", "j", "s"]);
    let kinds: Vec<ColumnKind> = cols.iter().map(|c| c.kind).collect();
    use ColumnKind::*;
    assert_eq!(kinds, [Bool, Int, Float, Text, Text, Text, Text]);
    let hints: Vec<DisplayHint> = cols.iter().map(|c| c.hint).collect();
    use DisplayHint::*;
    assert_eq!(hints, [Plain, Plain, Plain, Decimal, Temporal, Json, Plain]);

    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.len(), 1);
    assert_eq!(chunk.cell(0, 0), Cell::Bool(true));
    assert_eq!(chunk.cell(0, 1), Cell::Int(1));
    assert_eq!(chunk.cell(0, 2), Cell::Float(1.5));
    assert_eq!(chunk.cell(0, 3), Cell::Text("12345678901234567890.5"));
    assert_eq!(chunk.cell(0, 5), Cell::Text(r#"{"a": 1}"#));
    assert_eq!(chunk.cell(0, 6), Cell::Text("x"));
    s.close().await.unwrap();
}

#[tokio::test]
async fn text_values_round_trip() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let mut s = rows(
        &conn,
        r"SELECT NULL::text, E'a\nb', repeat('x', 1048576), '🚀 è 日本',
                 '\xdeadbeef'::bytea, 'NaN'::float8, 'Infinity'::float8, '-Infinity'::float8",
    )
    .await;
    assert_eq!(s.columns()[4].kind, ColumnKind::Bytes);
    assert_eq!(s.columns()[4].hint, DisplayHint::Binary);
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.len(), 1);
    assert_eq!(chunk.cell(0, 0), Cell::Null);
    assert_eq!(chunk.cell(0, 1), Cell::Text("a\nb"));
    match chunk.cell(0, 2) {
        Cell::Text(t) => assert_eq!(t.len(), 1_048_576),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(chunk.cell(0, 3), Cell::Text("🚀 è 日本"));
    assert_eq!(chunk.cell(0, 4), Cell::Bytes(&[0xde, 0xad, 0xbe, 0xef]));
    match chunk.cell(0, 5) {
        Cell::Float(f) => assert!(f.is_nan(), "{f}"),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(chunk.cell(0, 6), Cell::Float(f64::INFINITY));
    assert_eq!(chunk.cell(0, 7), Cell::Float(f64::NEG_INFINITY));
    s.close().await.unwrap();
}

#[tokio::test]
async fn ddl_and_dml_affected() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    assert_eq!(affected(&conn, "CREATE TEMP TABLE t(x int)").await, 0);
    assert_eq!(
        affected(&conn, "INSERT INTO t VALUES (1),(2);  \n").await,
        2
    );
}

#[tokio::test]
async fn returning_and_show_are_buffered() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    affected(&conn, "CREATE TEMP TABLE t(x int)").await;

    let mut s = rows(&conn, "INSERT INTO t VALUES (3) RETURNING x").await;
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.len(), 1);
    assert_eq!(chunk.cell(0, 0), Cell::Int(3));
    assert!(s.next_chunk(500).await.unwrap().is_none());
    s.close().await.unwrap();

    let mut s = rows(&conn, "SHOW server_version").await;
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.len(), 1);
    assert!(matches!(chunk.cell(0, 0), Cell::Text(v) if !v.is_empty()));
    assert!(s.next_chunk(500).await.unwrap().is_none());
    s.close().await.unwrap();

    // A data-modifying WITH cannot be a cursor: it falls back to buffering.
    let mut s = rows(
        &conn,
        "WITH ins AS (INSERT INTO t VALUES (4), (5) RETURNING x) SELECT x FROM ins ORDER BY x",
    )
    .await;
    let chunk = s.next_chunk(1).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Int(4));
    let chunk = s.next_chunk(1).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Int(5));
    assert!(s.next_chunk(1).await.unwrap().is_none());
    s.close().await.unwrap();

    // A top-level DML with a WITH clause is not a SelectStmt: DECLARE rejects it with 42601.
    let mut s = rows(
        &conn,
        "WITH v AS (SELECT 6 AS x) INSERT INTO t SELECT x FROM v RETURNING x",
    )
    .await;
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.len(), 1);
    assert_eq!(chunk.cell(0, 0), Cell::Int(6));
    assert!(s.next_chunk(500).await.unwrap().is_none());
    s.close().await.unwrap();
}

#[tokio::test]
async fn syntax_error_has_position() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let err = conn
        .relational()
        .unwrap()
        .execute("SELECT * FROM")
        .await
        .err()
        .expect("must fail");
    assert_eq!(err.kind, ErrorKind::Syntax, "{err:?}");
    assert_eq!(err.position, Some(13), "{err:?}");
}

#[tokio::test]
async fn cancel_running_fetch() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let mut s = rows(&conn, "SELECT pg_sleep(30), 1").await;
    let fetch = tokio::spawn(async move {
        let r = s.next_chunk(500).await.map(|c| c.map(|c| c.len()));
        (r, s)
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    conn.cancel_handle().cancel().await.unwrap();
    let (result, mut s) = tokio::time::timeout(Duration::from_secs(3), fetch)
        .await
        .expect("fetch must return promptly after cancel")
        .unwrap();
    let err = result.expect_err("fetch must fail");
    assert_eq!(err.kind, ErrorKind::Cancelled, "{err:?}");
    s.close().await.unwrap();
    drop(s);

    let mut s = rows(&conn, "SELECT 1").await;
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Int(1));
}

#[tokio::test]
async fn cancelled_stream_dropped_without_close_recovers() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let mut s = rows(&conn, "SELECT pg_sleep(30), 1").await;
    let fetch = tokio::spawn(async move { s.next_chunk(500).await.map(|_| ()) });
    tokio::time::sleep(Duration::from_millis(500)).await;
    conn.cancel_handle().cancel().await.unwrap();
    let err = tokio::time::timeout(Duration::from_secs(3), fetch)
        .await
        .unwrap()
        .unwrap()
        .expect_err("fetch must fail");
    assert_eq!(err.kind, ErrorKind::Cancelled, "{err:?}");

    let mut s = rows(&conn, "SELECT 1").await;
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Int(1));
}

#[tokio::test]
async fn execute_while_previous_cursor_open_closes_it() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let mut s = rows(&conn, "SELECT g FROM generate_series(1, 10000) g").await;
    assert_eq!(s.next_chunk(500).await.unwrap().unwrap().len(), 500);
    drop(s);

    let next = async {
        let mut s = rows(&conn, "SELECT 2").await;
        let chunk = s.next_chunk(500).await.unwrap().unwrap();
        assert_eq!(chunk.len(), 1);
        assert_eq!(chunk.cell(0, 0), Cell::Int(2));
        assert!(s.next_chunk(500).await.unwrap().is_none());
    };
    tokio::time::timeout(Duration::from_secs(2), next)
        .await
        .expect("next query must run promptly");
}

#[tokio::test]
async fn close_mid_cursor_ends_stream_and_frees_connection() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let mut a = rows(&conn, "SELECT g FROM generate_series(1, 10000) g").await;
    assert_eq!(a.next_chunk(500).await.unwrap().unwrap().len(), 500);
    a.close().await.unwrap();
    assert!(a.next_chunk(500).await.unwrap().is_none());

    // `a` is still alive: closing it must have released the connection.
    let next = async {
        let mut b = rows(&conn, "SELECT 2").await;
        let chunk = b.next_chunk(500).await.unwrap().unwrap();
        assert_eq!(chunk.cell(0, 0), Cell::Int(2));
    };
    tokio::time::timeout(Duration::from_secs(2), next)
        .await
        .expect("next query must run promptly");
    a.close().await.unwrap();
}

#[tokio::test]
async fn open_data_sql_runs_for_awkward_names() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    affected(&conn, r#"CREATE SCHEMA "select""#).await;
    affected(
        &conn,
        r#"CREATE TABLE "select"."Mixed ""Case"" T"(id int primary key, note text not null)"#,
    )
    .await;
    let node = Node {
        path: NodePath(
            ["select", "tables", "Mixed \"Case\" T"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ),
        name: "Mixed \"Case\" T".into(),
        kind: NodeKind::Table,
        has_children: true,
        detail: None,
    };
    let sql = conn
        .relational()
        .unwrap()
        .dialect()
        .select_all(&node, Some(500))
        .unwrap();
    let mut s = rows(&conn, &sql).await;
    let names: Vec<String> = s.columns().iter().map(|c| c.name.clone()).collect();
    assert_eq!(names, ["id", "note"]);
    assert!(s.next_chunk(500).await.unwrap().is_none());
    s.close().await.unwrap();
}

async fn single_int(conn: &Arc<dyn Connection>, sql: &str) -> i64 {
    let mut s = rows(conn, sql).await;
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    let Cell::Int(v) = chunk.cell(0, 0) else {
        panic!("{sql}: expected an int")
    };
    s.close().await.unwrap();
    v
}

async fn table_with_x_1(conn: &Arc<dyn Connection>) {
    affected(conn, "CREATE TEMP TABLE t(x int)").await;
    affected(conn, "INSERT INTO t VALUES (1)").await;
}

#[tokio::test]
async fn select_inside_user_transaction_keeps_it_open() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    table_with_x_1(&conn).await;
    affected(&conn, "BEGIN").await;
    affected(&conn, "UPDATE t SET x = 2").await;
    let mut s = rows(&conn, "SELECT x FROM t").await;
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Int(2));
    assert!(s.next_chunk(500).await.unwrap().is_none());
    s.close().await.unwrap();
    drop(s);
    affected(&conn, "ROLLBACK").await;
    assert_eq!(single_int(&conn, "SELECT x FROM t").await, 1);
}

#[tokio::test]
async fn dropped_partial_stream_inside_user_transaction_keeps_it_open() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    table_with_x_1(&conn).await;
    affected(&conn, "BEGIN").await;
    affected(&conn, "UPDATE t SET x = 2").await;
    let mut s = rows(&conn, "SELECT g FROM generate_series(1, 10000) g").await;
    assert_eq!(s.next_chunk(500).await.unwrap().unwrap().len(), 500);
    drop(s);
    affected(&conn, "ROLLBACK").await;
    assert_eq!(single_int(&conn, "SELECT x FROM t").await, 1);
}

#[tokio::test]
async fn non_cursor_select_inside_user_transaction_keeps_it_healthy() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    table_with_x_1(&conn).await;
    affected(&conn, "BEGIN").await;
    affected(&conn, "UPDATE t SET x = 2").await;
    // DECLARE rejects a data-modifying CTE; the fallback must not abort the user's block.
    let mut s = rows(
        &conn,
        "WITH ins AS (INSERT INTO t VALUES (5) RETURNING x) SELECT x FROM ins",
    )
    .await;
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Int(5));
    s.close().await.unwrap();
    drop(s);
    assert_eq!(single_int(&conn, "SELECT count(*)::int8 FROM t").await, 2);
    affected(&conn, "ROLLBACK").await;
    assert_eq!(single_int(&conn, "SELECT count(*)::int8 FROM t").await, 1);
}

#[tokio::test]
async fn cancelled_fetch_inside_user_transaction_leaves_block_to_user() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    table_with_x_1(&conn).await;
    affected(&conn, "BEGIN").await;
    affected(&conn, "UPDATE t SET x = 2").await;
    let mut s = rows(&conn, "SELECT pg_sleep(30), x FROM t").await;
    let fetch = tokio::spawn(async move {
        let r = s.next_chunk(500).await.map(|_| ());
        (r, s)
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    conn.cancel_handle().cancel().await.unwrap();
    let (result, mut s) = tokio::time::timeout(Duration::from_secs(3), fetch)
        .await
        .expect("fetch must return promptly after cancel")
        .unwrap();
    assert_eq!(
        result.expect_err("fetch must fail").kind,
        ErrorKind::Cancelled
    );
    s.close().await.unwrap();
    drop(s);

    // The block is still open (aborted), not silently ended by sqlr.
    let err = conn
        .relational()
        .unwrap()
        .execute("SELECT 1")
        .await
        .err()
        .expect("an aborted block rejects statements");
    assert!(err.message.contains("aborted"), "{err:?}");
    affected(&conn, "ROLLBACK").await;
    assert_eq!(single_int(&conn, "SELECT x FROM t").await, 1);
}

#[tokio::test]
async fn terminated_backend_is_connection_error() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (container, cfg, sec) = pg().await;
    let conn = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    let admin = PostgresDriver::new().connect(&cfg, &sec).await.unwrap();
    let pid = single_int(&conn, "SELECT pg_backend_pid()::int8").await;
    let mut s = rows(&conn, "SELECT pg_sleep(30), 1").await;
    let fetch = tokio::spawn(async move { s.next_chunk(500).await.map(|_| ()) });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut kill = rows(&admin, &format!("SELECT pg_terminate_backend({pid})")).await;
    let chunk = kill.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Bool(true));
    kill.close().await.unwrap();
    let err = tokio::time::timeout(Duration::from_secs(3), fetch)
        .await
        .expect("fetch must return promptly after termination")
        .unwrap()
        .expect_err("fetch must fail");
    assert_eq!(err.kind, ErrorKind::Connection, "{err:?}");
    drop(container);
}

async fn exec_with(conn: &Arc<dyn Connection>, sql: &str, params: &[Value]) -> Execution {
    conn.relational()
        .unwrap()
        .execute_with(sql, params)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"))
}

async fn rows_with(conn: &Arc<dyn Connection>, sql: &str, params: &[Value]) -> Box<dyn RowStream> {
    match exec_with(conn, sql, params).await {
        Execution::Rows(s) => s,
        Execution::Affected(n) => panic!("{sql}: expected rows, got Affected({n})"),
    }
}

#[tokio::test]
async fn hints_for_typed_values() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let mut s = rows(
        &conn,
        r#"SELECT 1.5::numeric, gen_random_uuid(), '<a/>'::xml, ARRAY[1,2], '1 day'::interval,
                  '\x01'::bytea, 1::int4"#,
    )
    .await;
    let hints: Vec<DisplayHint> = s.columns().iter().map(|c| c.hint).collect();
    use DisplayHint::*;
    assert_eq!(hints, [Decimal, Uuid, Xml, Array, Interval, Binary, Plain]);
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 3), Cell::Text("{1,2}"));
    assert_eq!(chunk.cell(0, 5), Cell::Bytes(&[1]));
    s.close().await.unwrap();
}

#[tokio::test]
async fn execute_with_binds_every_value_kind() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let rel = conn.relational().unwrap();
    assert_eq!(rel.dialect().placeholder(1), "$1");
    assert_eq!(rel.dialect().placeholder(12), "$12");

    let params = [
        Value::Null,
        Value::Bool(true),
        Value::Int(-7),
        Value::Float(2.5),
        Value::Text("è😀 'q' \"d\"".into()),
        Value::Bytes(vec![0, 1, 255]),
        // Text and integers into types the driver has no binary encoding for: sent as text.
        Value::Text("12.50".into()),
        Value::Text("6b8f2e4c-1f0a-4d6e-9a3b-0c1d2e3f4a5b".into()),
        Value::Int(3),
        Value::Text(r#"{"a": 1}"#.into()),
        Value::Text("2026-10-21".into()),
        Value::Int(5),
        Value::Float(f64::INFINITY),
    ];
    let mut s = rows_with(
        &conn,
        "SELECT $1::int4, $2::bool, $3::int8, $4::float8, $5::text, $6::bytea, $7::numeric, \
                $8::uuid, $9::int2, $10::jsonb, $11::date, $12::numeric, $13::float4",
        &params,
    )
    .await;
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    let cells: Vec<Cell<'_>> = (0..13).map(|c| chunk.cell(0, c)).collect();
    assert_eq!(
        cells,
        [
            Cell::Null,
            Cell::Bool(true),
            Cell::Int(-7),
            Cell::Float(2.5),
            Cell::Text("è😀 'q' \"d\""),
            Cell::Bytes(&[0, 1, 255]),
            Cell::Text("12.50"),
            Cell::Text("6b8f2e4c-1f0a-4d6e-9a3b-0c1d2e3f4a5b"),
            Cell::Int(3),
            Cell::Text(r#"{"a": 1}"#),
            Cell::Text("2026-10-21"),
            Cell::Text("5"),
            Cell::Float(f64::INFINITY),
        ]
    );
    s.close().await.unwrap();

    // Writes return affected counts; values reach typed columns.
    affected(
        &conn,
        "CREATE TEMP TABLE p(id int2 PRIMARY KEY, amount numeric, at timestamptz, data bytea, ok bool)",
    )
    .await;
    affected(&conn, "SET TIME ZONE 'UTC'").await;
    let insert = "INSERT INTO p VALUES ($1, $2, $3, $4, $5)";
    let row = [
        Value::Int(1),
        Value::Float(0.1),
        Value::Text("2026-10-21 10:00:00+00".into()),
        Value::Bytes(b"\x00bin".to_vec()),
        Value::Null,
    ];
    match exec_with(&conn, insert, &row).await {
        Execution::Affected(n) => assert_eq!(n, 1),
        Execution::Rows(_) => panic!("insert returned rows"),
    }
    let err = rel
        .execute_with(
            insert,
            &[
                Value::Int(70_000),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
            ],
        )
        .await
        .err()
        .expect("out of range for int2");
    assert!(err.message.contains("out of range"), "{err:?}");

    // A row-returning write with parameters cannot be a cursor: it is buffered.
    let mut s = rows_with(
        &conn,
        "UPDATE p SET amount = amount + $1 WHERE id = $2 RETURNING id, amount, at, data, ok",
        &[Value::Int(2), Value::Int(1)],
    )
    .await;
    let kinds: Vec<ColumnKind> = s.columns().iter().map(|c| c.kind).collect();
    use ColumnKind::*;
    assert_eq!(kinds, [Int, Text, Text, Bytes, Bool]);
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Int(1));
    assert_eq!(chunk.cell(0, 1), Cell::Text("2.1"));
    assert_eq!(chunk.cell(0, 2), Cell::Text("2026-10-21 10:00:00+00"));
    assert_eq!(chunk.cell(0, 3), Cell::Bytes(b"\x00bin"));
    assert_eq!(chunk.cell(0, 4), Cell::Null);
    assert!(s.next_chunk(500).await.unwrap().is_none());
    s.close().await.unwrap();

    // A data-modifying WITH with parameters is buffered too.
    let mut s = rows_with(
        &conn,
        "WITH d AS (DELETE FROM p WHERE id = $1 RETURNING id, ok) SELECT * FROM d",
        &[Value::Int(1)],
    )
    .await;
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Int(1));
    assert_eq!(chunk.cell(0, 1), Cell::Null);
    s.close().await.unwrap();

    // Parameters work inside the user's transaction block, and the block stays healthy.
    affected(&conn, "BEGIN").await;
    let mut s = rows_with(&conn, "SELECT $1::int8 + 1", &[Value::Int(1)]).await;
    assert_eq!(
        s.next_chunk(500).await.unwrap().unwrap().cell(0, 0),
        Cell::Int(2)
    );
    s.close().await.unwrap();
    affected(&conn, "COMMIT").await;
}

fn origin(schema: &str, table: &str, column: &str) -> Option<ColumnOrigin> {
    Some(ColumnOrigin {
        table: TableRef {
            database: None,
            schema: Some(schema.into()),
            name: table.into(),
        },
        column: column.into(),
    })
}

#[tokio::test]
async fn origins_on_table_select() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    affected(&conn, r#"CREATE SCHEMA "S 1""#).await;
    affected(
        &conn,
        r#"CREATE TABLE "S 1"."Items"(id int PRIMARY KEY, "Name" text, dropped int)"#,
    )
    .await;
    affected(&conn, r#"ALTER TABLE "S 1"."Items" DROP COLUMN dropped"#).await;
    affected(&conn, r#"ALTER TABLE "S 1"."Items" ADD COLUMN extra int"#).await;
    affected(
        &conn,
        "CREATE TABLE public.owners(id int PRIMARY KEY, item int)",
    )
    .await;

    let s = rows(&conn, r#"SELECT * FROM "S 1"."Items""#).await;
    let origins: Vec<_> = s.columns().iter().map(|c| c.origin.clone()).collect();
    assert_eq!(
        origins,
        [
            origin("S 1", "Items", "id"),
            origin("S 1", "Items", "Name"),
            origin("S 1", "Items", "extra"),
        ]
    );
    drop(s);

    let s = rows_with(
        &conn,
        r#"SELECT i.id, o.id AS owner, i."Name" || 'x' AS computed, 1 AS k
           FROM "S 1"."Items" i JOIN owners o ON o.item = i.id WHERE i.id > $1"#,
        &[Value::Int(0)],
    )
    .await;
    let origins: Vec<_> = s.columns().iter().map(|c| c.origin.clone()).collect();
    assert_eq!(
        origins,
        [
            origin("S 1", "Items", "id"),
            origin("public", "owners", "id"),
            None,
            None,
        ]
    );
    drop(s);

    // A renamed table is reported by its new name.
    affected(&conn, r#"ALTER TABLE "S 1"."Items" RENAME TO "Things""#).await;
    let s = rows(&conn, r#"SELECT id FROM "S 1"."Things""#).await;
    assert_eq!(s.columns()[0].origin, origin("S 1", "Things", "id"));
}

#[tokio::test]
async fn notices_are_collected() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let rel = conn.relational().unwrap();
    assert!(rel.take_notices().await.is_empty());
    affected(
        &conn,
        "DO $$ BEGIN RAISE NOTICE 'hello %', 1; RAISE WARNING 'careful'; END $$",
    )
    .await;
    assert_eq!(
        rel.take_notices().await,
        ["NOTICE: hello 1", "WARNING: careful"]
    );
    assert!(rel.take_notices().await.is_empty());
    // `DROP ... IF EXISTS` of a missing object notices too.
    affected(&conn, "DROP TABLE IF EXISTS nope").await;
    let notices = rel.take_notices().await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("does not exist"), "{notices:?}");
}

#[tokio::test]
async fn quote_ident_round_trips_per_dialect() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let rel = conn.relational().unwrap();
    let d = rel.dialect();
    for name in ["Order", "my table", "select", "a\"b", "a`b"] {
        let table = TableRef {
            database: None,
            schema: Some("public".into()),
            name: name.into(),
        };
        let q = d.qualified(&table);
        let col = d.quote_ident(name);
        affected(&conn, &format!("CREATE TABLE {q} ({col} text PRIMARY KEY)")).await;
        let insert = format!("INSERT INTO {q} ({col}) VALUES ({})", d.placeholder(1));
        match exec_with(&conn, &insert, &[Value::Text(name.into())]).await {
            Execution::Affected(n) => assert_eq!(n, 1, "{name}"),
            Execution::Rows(_) => panic!("{name}: insert returned rows"),
        }
        let mut s = rows(&conn, &format!("SELECT {col} FROM {q}")).await;
        assert_eq!(s.columns()[0].name, name);
        assert_eq!(s.columns()[0].origin, origin("public", name, name));
        let chunk = s.next_chunk(500).await.unwrap().unwrap();
        assert_eq!(chunk.cell(0, 0), Cell::Text(name));
        s.close().await.unwrap();
        let info = rel.describe(&table).await.unwrap();
        assert_eq!(info.primary_key, [name.to_string()], "{name}");
        let wrapped = d.wrap_order(&format!("SELECT * FROM {q}"), &[(name.to_string(), true)]);
        rows(&conn, &wrapped).await.close().await.unwrap();
    }
}

#[tokio::test]
async fn literals_and_read_only_are_accepted_by_the_server() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let d = conn.relational().unwrap().dialect();
    let sql = format!(
        "SELECT {}, {}",
        d.string_literal("it's \\ fine"),
        d.bytes_literal(&[0, 0xab, 0xff])
    );
    let mut s = rows(&conn, &sql).await;
    let chunk = s.next_chunk(500).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Text("it's \\ fine"));
    assert_eq!(chunk.cell(0, 1), Cell::Bytes(&[0, 0xab, 0xff]));
    s.close().await.unwrap();

    // Not a temporary table: read-only transactions may still write those.
    affected(&conn, "CREATE TABLE ro(x int)").await;
    affected(&conn, &d.read_only_sql(true).unwrap()).await;
    let err = conn
        .relational()
        .unwrap()
        .execute("INSERT INTO ro VALUES (1)")
        .await
        .err()
        .expect("read-only session refuses writes");
    assert!(err.message.contains("read-only"), "{err:?}");
    affected(&conn, &d.read_only_sql(false).unwrap()).await;
    assert_eq!(affected(&conn, "INSERT INTO ro VALUES (1)").await, 1);
}

/// The session read-only switch is enforced by the server without the engine's classifier,
/// and a failure inside a transaction aborts it until a rollback (`error_aborts_transaction`).
#[tokio::test]
async fn read_only_enforced_by_server() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let d = conn.relational().unwrap().dialect();
    assert!(d.error_aborts_transaction());
    affected(&conn, "CREATE TABLE ro2(x int)").await;
    affected(&conn, &d.read_only_sql(true).unwrap()).await;
    let rel = conn.relational().unwrap();
    let err = rel
        .execute("INSERT INTO ro2 VALUES (1)")
        .await
        .err()
        .expect("refused");
    assert!(err.message.contains("read-only"), "{err:?}");
    affected(&conn, &d.read_only_sql(false).unwrap()).await;

    affected(&conn, d.begin_sql()).await;
    assert!(rel.execute("SELECT nope").await.is_err());
    let err = rel
        .execute("INSERT INTO ro2 VALUES (1)")
        .await
        .err()
        .expect("aborted");
    assert!(err.message.contains("aborted"), "{err:?}");
    affected(&conn, "ROLLBACK").await;
    assert_eq!(affected(&conn, "INSERT INTO ro2 VALUES (1)").await, 1);
}

/// Export `INSERT`s and DDL comments mean the same whatever `standard_conforming_strings` is.
#[tokio::test]
async fn literals_read_back_with_either_string_setting() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_c, conn) = connect().await;
    let d = conn.relational().unwrap().dialect();
    let texts = [
        "plain",
        "it's",
        "a\\b",
        "\\'",
        "ends with \\",
        "\\n is not a newline",
        "😀 tab\there",
        "",
    ];
    affected(
        &conn,
        "CREATE TEMP TABLE lit (id int PRIMARY KEY, v text, b bytea)",
    )
    .await;
    for setting in ["on", "off"] {
        affected(
            &conn,
            &format!("SET standard_conforming_strings = {setting}"),
        )
        .await;
        affected(&conn, "DELETE FROM lit").await;
        for (i, text) in texts.iter().enumerate() {
            let sql = format!(
                "INSERT INTO lit VALUES ({i}, {}, {})",
                d.string_literal(text),
                d.bytes_literal(text.as_bytes())
            );
            affected(&conn, &sql).await;
        }
        let mut s = rows(&conn, "SELECT v, b FROM lit ORDER BY id").await;
        let mut got = Vec::new();
        while let Some(chunk) = s.next_chunk(100).await.unwrap() {
            for r in 0..chunk.len() {
                got.push((
                    Value::from_cell(chunk.cell(r, 0)),
                    Value::from_cell(chunk.cell(r, 1)),
                ));
            }
        }
        s.close().await.unwrap();
        let expected: Vec<(Value, Value)> = texts
            .iter()
            .map(|t| {
                (
                    Value::Text(t.to_string()),
                    Value::Bytes(t.as_bytes().to_vec()),
                )
            })
            .collect();
        assert_eq!(got, expected, "standard_conforming_strings = {setting}");
    }
}

#[tokio::test]
async fn pg_application_name_applied() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let (_container, mut cfg, secrets) = pg().await;
    cfg.values.insert(
        "property.application_name".into(),
        "sqlr property test".into(),
    );
    cfg.values
        .insert("property.statement_timeout".into(), "1234".into());
    let conn = PostgresDriver::new().connect(&cfg, &secrets).await.unwrap();
    let mut stream=rows(&conn,"SELECT current_setting('application_name') AS app, current_setting('statement_timeout') AS timeout").await;
    let chunk = stream.next_chunk(1).await.unwrap().unwrap();
    assert_eq!(chunk.cell(0, 0), Cell::Text("sqlr property test"));
    assert_eq!(chunk.cell(0, 1), Cell::Text("1234ms"));
    stream.close().await.unwrap();
}
