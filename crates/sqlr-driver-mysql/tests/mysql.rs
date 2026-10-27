//! The MySQL driver against MySQL 8.4 and MariaDB 11.4 containers. Every check runs on
//! both servers (`mysql84::*`, `mariadb114::*`).

use crate::support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlr_core::{
    Cell, ColumnKind, Connection, ConnectionConfig, DisplayHint, Driver, ErrorKind, Execution,
    Node, NodeKind, NodePath, RelationKind, RowStream, Secrets, TableRef, Value,
};
use sqlr_driver_mysql::MysqlDriver;
use support::{Server, config, secrets, start, start_isolated, start_without_tls};

macro_rules! suite {
    ($($name:ident),* $(,)?) => {
        mod mysql84 {
            $(#[tokio::test(flavor = "multi_thread")]
            async fn $name() {
                sqlr_testkit::skip_container_tests!("MYSQL");
                super::$name(super::Server::MySql).await
            })*
        }
        mod mariadb114 {
            $(#[tokio::test(flavor = "multi_thread")]
            async fn $name() {
                sqlr_testkit::skip_container_tests!("MARIADB");
                super::$name(super::Server::MariaDb).await
            })*
        }
    };
}

suite!(
    connect_and_ping,
    wrong_password_is_auth_error,
    browse_tree_with_system_folder,
    browse_only_current_database,
    streams_100k_rows,
    closing_a_partial_stream_frees_the_session,
    procedure_call_returns_rows,
    kill_query_cancels_within_2s,
    typed_values_matrix,
    origins_on_table_select,
    describe_keys,
    ddl_of_objects,
    execute_with_binds_every_value_kind,
    catalog_lists_relations,
    connect_database_opens_other_database,
    warnings_become_notices,
    quote_ident_round_trips_per_dialect,
    tls_required_encrypts,
    tls_required_but_unavailable,
    read_only_enforced_by_server,
    exported_literals_read_back_in_any_sql_mode,
);

fn path(segs: &[&str]) -> NodePath {
    NodePath(segs.iter().map(|s| s.to_string()).collect())
}

fn names(nodes: &[Node]) -> Vec<&str> {
    nodes.iter().map(|n| n.name.as_str()).collect()
}

async fn connect(cfg: &ConnectionConfig, sec: &Secrets) -> Arc<dyn Connection> {
    MysqlDriver::new()
        .connect(cfg, sec)
        .await
        .unwrap_or_else(|e| panic!("connect: {e:?}"))
}

async fn exec(conn: &Arc<dyn Connection>, sql: &str) -> Execution {
    conn.relational()
        .unwrap()
        .execute(sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"))
}

/// Runs each statement of a setup script, draining results.
async fn run(conn: &Arc<dyn Connection>, script: &[&str]) {
    for sql in script {
        if let Execution::Rows(mut s) = exec(conn, sql).await {
            while s.next_chunk(1000).await.unwrap().is_some() {}
            s.close().await.unwrap();
        }
    }
}

async fn rows(conn: &Arc<dyn Connection>, sql: &str) -> Box<dyn RowStream> {
    match exec(conn, sql).await {
        Execution::Rows(s) => s,
        Execution::Affected(n) => panic!("{sql}: expected rows, got Affected({n})"),
    }
}

/// All rows of `stream` as owned values.
async fn collect(mut stream: Box<dyn RowStream>) -> Vec<Vec<Value>> {
    let mut out = Vec::new();
    while let Some(chunk) = stream.next_chunk(1000).await.unwrap() {
        for r in 0..chunk.len() {
            out.push(
                (0..chunk.columns().len())
                    .map(|c| Value::from_cell(chunk.cell(r, c)))
                    .collect(),
            );
        }
    }
    stream.close().await.unwrap();
    out
}

async fn query(conn: &Arc<dyn Connection>, sql: &str) -> Vec<Vec<Value>> {
    collect(rows(conn, sql).await).await
}

/// `shop` with a table, a view, a function, a procedure and a trigger.
async fn shop(conn: &Arc<dyn Connection>) {
    run(
        conn,
        &[
            "CREATE DATABASE shop",
            "CREATE TABLE shop.orders (id INT PRIMARY KEY, note VARCHAR(20) NOT NULL, qty INT)",
            "INSERT INTO shop.orders VALUES (1, 'a', 2), (2, 'b', NULL)",
            "CREATE VIEW shop.v_orders AS SELECT id, note FROM shop.orders",
            "CREATE FUNCTION shop.f_one() RETURNS INT DETERMINISTIC RETURN 1",
            "CREATE PROCEDURE shop.p_one() SELECT 1",
            "CREATE FUNCTION shop.same() RETURNS INT DETERMINISTIC RETURN 2",
            "CREATE PROCEDURE shop.same() SELECT 3",
            "CREATE TRIGGER shop.trg BEFORE INSERT ON shop.orders FOR EACH ROW SET NEW.qty = COALESCE(NEW.qty, 0)",
        ],
    )
    .await;
}

/// The session read-only switch (`read_only_sql`) is enforced by the server itself, without
/// the engine's classifier.
async fn read_only_enforced_by_server(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let conn = connect(&cfg, &sec).await;
    run(&conn, &["CREATE TABLE ro (x INT)"]).await;
    let rel = conn.relational().unwrap();
    run(
        &conn,
        &[rel.dialect().read_only_sql(true).unwrap().as_str()],
    )
    .await;
    let Err(err) = rel.execute("INSERT INTO ro VALUES (1)").await else {
        panic!("write allowed on a read-only session")
    };
    assert!(err.message.contains("READ ONLY"), "{err:?}");
    run(
        &conn,
        &[rel.dialect().read_only_sql(false).unwrap().as_str()],
    )
    .await;
    run(&conn, &["INSERT INTO ro VALUES (1)"]).await;
}

async fn connect_and_ping(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let conn = connect(&cfg, &sec).await;
    let info = conn.ping().await.unwrap();
    assert_eq!(info.product, server.product(), "{info:?}");
    assert!(!info.version.is_empty());
    assert_eq!(conn.database_name(), Some(cfg.values["database"].as_str()));
}

#[tokio::test]
async fn unreachable_host_is_connection_error() {
    sqlr_testkit::skip_container_tests!("MYSQL");
    let err = MysqlDriver::new()
        .connect(&config("127.0.0.1", 1), &secrets("x"))
        .await
        .err()
        .expect("must fail");
    assert_eq!(err.kind, ErrorKind::Connection, "{err:?}");
}

async fn wrong_password_is_auth_error(server: Server) {
    let (_c, cfg, sec) = start_isolated(server).await;
    let root = connect(&cfg, &sec).await;
    run(
        &root,
        &[
            "CREATE USER 'bob'@'%' IDENTIFIED BY 'right'",
            "GRANT ALL ON test.* TO 'bob'@'%'",
        ],
    )
    .await;
    let mut bob = cfg.clone();
    bob.values.insert("user".into(), "bob".into());
    let err = MysqlDriver::new()
        .connect(&bob, &secrets("wrong"))
        .await
        .err()
        .expect("must fail");
    assert_eq!(err.kind, ErrorKind::Auth, "{err:?}");
    connect(&bob, &secrets("right")).await.ping().await.unwrap();
}

async fn browse_tree_with_system_folder(server: Server) {
    let (_c, cfg, sec) = start_isolated(server).await;
    let conn = connect(&cfg, &sec).await;
    shop(&conn).await;

    let root = conn.browse(&NodePath::root()).await.unwrap();
    assert_eq!(names(&root), ["shop", "test", "System"]);
    assert_eq!(root[1].detail.as_deref(), Some("current"));
    assert!(root[..2].iter().all(|n| n.kind == NodeKind::Schema));
    let system = &root[2];
    assert_eq!((system.kind, system.has_children), (NodeKind::Folder, true));
    let schemas = conn.browse(&system.path).await.unwrap();
    assert_eq!(
        names(&schemas),
        ["information_schema", "mysql", "performance_schema", "sys"]
    );
    // System schemas browse like any database.
    let mysql = &schemas[1];
    assert_eq!(mysql.path, path(&["mysql"]));
    assert_eq!(conn.database_for_path(&system.path), None);
    assert_eq!(conn.database_for_path(&mysql.path), Some("mysql".into()));
    let mysql_tables = conn.browse(&path(&["mysql", "tables"])).await.unwrap();
    assert!(!mysql_tables.is_empty());

    let folders = conn.browse(&path(&["shop"])).await.unwrap();
    assert_eq!(
        names(&folders),
        ["tables", "views", "functions", "procedures", "triggers"]
    );
    let tables = conn.browse(&path(&["shop", "tables"])).await.unwrap();
    assert_eq!(names(&tables), ["orders"]);
    assert_eq!(tables[0].kind, NodeKind::Table);
    let parts = conn.browse(&tables[0].path).await.unwrap();
    assert_eq!(names(&parts), ["columns", "indexes"]);
    let columns = conn
        .browse(&path(&["shop", "tables", "orders", "columns"]))
        .await
        .unwrap();
    assert_eq!(names(&columns), ["id", "note", "qty"]);
    // MariaDB still prints display widths.
    let int = if server == Server::MariaDb {
        "int(11)"
    } else {
        "int"
    };
    assert_eq!(columns[0].detail, Some(format!("{int} PK NOT NULL")));
    assert_eq!(columns[1].detail.as_deref(), Some("varchar(20) NOT NULL"));
    assert_eq!(columns[2].detail.as_deref(), Some(int));
    let indexes = conn
        .browse(&path(&["shop", "tables", "orders", "indexes"]))
        .await
        .unwrap();
    assert_eq!(names(&indexes), ["PRIMARY"]);
    assert_eq!(indexes[0].detail.as_deref(), Some("UNIQUE (id)"));

    let views = conn.browse(&path(&["shop", "views"])).await.unwrap();
    assert_eq!(names(&views), ["v_orders"]);
    assert_eq!(views[0].kind, NodeKind::View);
    let view_columns = conn.browse(&views[0].path).await.unwrap();
    assert_eq!(names(&view_columns), ["id", "note"]);

    // A function and a procedure may share a name; each has its own folder and path.
    let functions = conn.browse(&path(&["shop", "functions"])).await.unwrap();
    assert_eq!(names(&functions), ["f_one", "same"]);
    assert!(functions.iter().all(|n| n.kind == NodeKind::Function));
    let procedures = conn.browse(&path(&["shop", "procedures"])).await.unwrap();
    assert_eq!(names(&procedures), ["p_one", "same"]);
    assert!(procedures.iter().all(|n| n.kind == NodeKind::Procedure));
    assert_ne!(functions[1].path, procedures[1].path);
    let rel = conn.relational().unwrap();
    let function_ddl = rel.ddl(&functions[1]).await.unwrap();
    assert!(
        function_ddl.contains(" FUNCTION ") && function_ddl.contains("RETURN 2"),
        "{function_ddl}"
    );
    let procedure_ddl = rel.ddl(&procedures[1]).await.unwrap();
    assert!(
        procedure_ddl.contains(" PROCEDURE ") && procedure_ddl.contains("SELECT 3"),
        "{procedure_ddl}"
    );
    let triggers = conn.browse(&path(&["shop", "triggers"])).await.unwrap();
    assert_eq!(names(&triggers), ["trg"]);
    assert_eq!(
        triggers[0].detail.as_deref(),
        Some("BEFORE INSERT ON orders")
    );

    for p in [
        path(&["shop", "nope"]),
        path(&["shop", "tables", "orders", "weird"]),
    ] {
        let err = conn.browse(&p).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unsupported, "{p:?}");
    }

    // Opening data from the explorer.
    let sql = conn
        .relational()
        .unwrap()
        .dialect()
        .select_all(&tables[0], Some(10))
        .unwrap();
    assert_eq!(query(&conn, &sql).await.len(), 2);
}

async fn browse_only_current_database(server: Server) {
    let (_c, mut cfg, sec) = start_isolated(server).await;
    let setup = connect(&cfg, &sec).await;
    shop(&setup).await;
    cfg.values
        .insert("show_all_databases".into(), "false".into());
    let conn = connect(&cfg, &sec).await;
    let root = conn.browse(&NodePath::root()).await.unwrap();
    assert_eq!(names(&root), ["test", "System"]);
    // Without a database every one is listed.
    cfg.values.insert("database".into(), String::new());
    let conn = connect(&cfg, &sec).await;
    assert_eq!(conn.database_name(), None);
    let root = conn.browse(&NodePath::root()).await.unwrap();
    assert_eq!(names(&root), ["shop", "test", "System"]);
}

const DIGITS: &str = "(SELECT 0 n UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 \
    UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 \
    UNION ALL SELECT 8 UNION ALL SELECT 9)";

/// 100 000 rows, `n` from 0 to 99 999.
fn hundred_thousand() -> String {
    format!(
        "SELECT a.n + 10*b.n + 100*c.n + 1000*d.n + 10000*e.n AS n \
         FROM {DIGITS} a, {DIGITS} b, {DIGITS} c, {DIGITS} d, {DIGITS} e"
    )
}

async fn streams_100k_rows(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let conn = connect(&cfg, &sec).await;
    let mut s = rows(&conn, &hundred_thousand()).await;
    assert_eq!(s.columns()[0].name, "n");
    assert_eq!(s.columns()[0].kind, ColumnKind::Int);
    let (mut chunks, mut total, mut sum) = (0, 0, 0i64);
    while let Some(chunk) = s.next_chunk(5000).await.unwrap() {
        assert!(chunk.len() <= 5000);
        chunks += 1;
        total += chunk.len();
        for r in 0..chunk.len() {
            let Cell::Int(n) = chunk.cell(r, 0) else {
                panic!("not an int")
            };
            sum += n;
        }
    }
    assert_eq!((chunks, total, sum), (20, 100_000, 4_999_950_000));
    assert!(s.next_chunk(5000).await.unwrap().is_none());
    s.close().await.unwrap();
    s.close().await.unwrap();
    assert_eq!(query(&conn, "SELECT 1").await, [[Value::Int(1)]]);
}

async fn closing_a_partial_stream_frees_the_session(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let conn = connect(&cfg, &sec).await;
    // Far more rows than are read: closing must not read them all.
    let big = format!(
        "{} CROSS JOIN {DIGITS} f CROSS JOIN {DIGITS} g",
        hundred_thousand()
    );
    let mut s = rows(&conn, &big).await;
    assert_eq!(s.next_chunk(100).await.unwrap().unwrap().len(), 100);
    let started = Instant::now();
    s.close().await.unwrap();
    assert_eq!(query(&conn, "SELECT 2").await, [[Value::Int(2)]]);
    // A stream dropped without close is finished by the next statement.
    let mut s = rows(&conn, &big).await;
    assert!(s.next_chunk(10).await.unwrap().is_some());
    drop(s);
    assert_eq!(query(&conn, "SELECT 3").await, [[Value::Int(3)]]);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // A small result is already complete on the server when it is closed early: the
    // `KILL QUERY` that closing sends must not hit the statements that follow.
    for _ in 0..5 {
        let mut s = rows(&conn, &format!("SELECT a.n FROM {DIGITS} a, {DIGITS} b")).await;
        assert_eq!(s.next_chunk(10).await.unwrap().unwrap().len(), 10);
        s.close().await.unwrap();
        assert_eq!(
            query(&conn, "SELECT SLEEP(0.2), 4").await,
            [[Value::Int(0), Value::Int(4)]]
        );
    }
}

async fn procedure_call_returns_rows(server: Server) {
    let (_c, cfg, sec) = start_isolated(server).await;
    let conn = connect(&cfg, &sec).await;
    shop(&conn).await;
    // The row set is followed by the call's own OK packet, which is read and dropped.
    assert_eq!(query(&conn, "CALL shop.p_one()").await, [[Value::Int(1)]]);
    assert_eq!(query(&conn, "SELECT 5").await, [[Value::Int(5)]]);
    run(&conn, &["CREATE PROCEDURE shop.p_none() DO 1"]).await;
    match exec(&conn, "CALL shop.p_none()").await {
        Execution::Affected(_) => {}
        Execution::Rows(_) => panic!("rows from a procedure without results"),
    }
    assert_eq!(query(&conn, "SELECT 6").await, [[Value::Int(6)]]);
}

async fn kill_query_cancels_within_2s(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let conn = connect(&cfg, &sec).await;
    let cancel = conn.cancel_handle();
    let runner = conn.clone();
    let heavy = format!(
        "SELECT COUNT(*) FROM {DIGITS} a, {DIGITS} b, {DIGITS} c, {DIGITS} d, {DIGITS} e, \
         {DIGITS} f, {DIGITS} g, {DIGITS} h, {DIGITS} i, {DIGITS} j"
    );
    let task = tokio::spawn(async move {
        match runner.relational().unwrap().execute(&heavy).await {
            Ok(Execution::Rows(mut s)) => s.next_chunk(10).await.map(|_| ()),
            Ok(Execution::Affected(_)) => Ok(()),
            Err(e) => Err(e),
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let started = Instant::now();
    cancel.cancel().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("cancelled within 2 s")
        .unwrap();
    let err = result.expect_err("the query was cancelled");
    assert_eq!(err.kind, ErrorKind::Cancelled, "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(query(&conn, "SELECT 1").await, [[Value::Int(1)]]);
}

async fn typed_values_matrix(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let conn = connect(&cfg, &sec).await;
    run(
        &conn,
        &[
            "CREATE TABLE typed (
                id INT PRIMARY KEY, d DECIMAL(20,5), j JSON, b1 BIT(1), b10 BIT(10), bl BLOB,
                vb VARBINARY(8), dt DATETIME(3), da DATE, ti TIME, y YEAR,
                e ENUM('small','large'), s SET('a','b','c'), g GEOMETRY,
                ub BIGINT UNSIGNED, f FLOAT, db DOUBLE, t TINYINT, v VARCHAR(20)
            )",
            "INSERT INTO typed VALUES (
                1, 12345678901234.56789, '{\"a\": 1}', b'1', b'1000000001', X'00FF10',
                X'CAFE', '2024-02-29 13:14:15.123', '2024-02-29', '-838:59:59', 2024,
                'large', 'a,c', ST_GeomFromText('POINT(1 2)'),
                18446744073709551615, 1.1, 2.5, -128, '😀 è'
            )",
            "INSERT INTO typed (id) VALUES (2)",
        ],
    )
    .await;
    let sql = "SELECT * FROM typed ORDER BY id";
    let s = rows(&conn, sql).await;
    let cols = s.columns();
    let shape: Vec<(&str, ColumnKind, DisplayHint)> = cols
        .iter()
        .map(|c| (c.name.as_str(), c.kind, c.hint))
        .collect();
    use ColumnKind as K;
    use DisplayHint as H;
    // MariaDB's JSON is LONGTEXT with a check constraint: plain text in the result.
    let json_hint = if server == Server::MySql {
        H::Json
    } else {
        H::Plain
    };
    assert_eq!(
        shape,
        [
            ("id", K::Int, H::Plain),
            ("d", K::Text, H::Decimal),
            ("j", K::Text, json_hint),
            ("b1", K::Bool, H::Plain),
            ("b10", K::Int, H::Plain),
            ("bl", K::Bytes, H::Binary),
            ("vb", K::Bytes, H::Binary),
            ("dt", K::Text, H::Temporal),
            ("da", K::Text, H::Temporal),
            ("ti", K::Text, H::Temporal),
            ("y", K::Int, H::Plain),
            ("e", K::Text, H::Plain),
            ("s", K::Text, H::Plain),
            ("g", K::Bytes, H::Geometry),
            ("ub", K::Text, H::Decimal),
            ("f", K::Float, H::Plain),
            ("db", K::Float, H::Plain),
            ("t", K::Int, H::Plain),
            ("v", K::Text, H::Plain),
        ]
    );
    let text = collect(s).await;
    let expected_first = vec![
        Value::Int(1),
        Value::Text("12345678901234.56789".into()),
        Value::Text("{\"a\": 1}".into()),
        Value::Bool(true),
        Value::Int(513),
        Value::Bytes(vec![0, 0xff, 0x10]),
        Value::Bytes(vec![0xca, 0xfe]),
        Value::Text("2024-02-29 13:14:15.123".into()),
        Value::Text("2024-02-29".into()),
        Value::Text("-838:59:59".into()),
        Value::Int(2024),
        Value::Text("large".into()),
        Value::Text("a,c".into()),
    ];
    assert_eq!(text[0][..13], expected_first[..]);
    // WKB with the SRID prefix: 4 + 21 bytes for a point.
    let Value::Bytes(g) = &text[0][13] else {
        panic!("geometry is bytes")
    };
    assert_eq!(g.len(), 25);
    assert_eq!(
        text[0][14..],
        [
            Value::Text("18446744073709551615".into()),
            Value::Float(1.1),
            Value::Float(2.5),
            Value::Int(-128),
            Value::Text("😀 è".into()),
        ]
    );
    assert!(text[1][1..].iter().all(Value::is_null), "{:?}", text[1]);

    // The binary protocol (prepared statements) decodes to the same cells.
    let binary = match conn
        .relational()
        .unwrap()
        .execute_with(sql, &[])
        .await
        .unwrap()
    {
        Execution::Rows(s) => collect(s).await,
        Execution::Affected(n) => panic!("affected {n}"),
    };
    assert_eq!(binary, text);
}

async fn origins_on_table_select(server: Server) {
    let (_c, cfg, sec) = start_isolated(server).await;
    let conn = connect(&cfg, &sec).await;
    shop(&conn).await;
    let s = rows(
        &conn,
        "SELECT o.id, o.note AS label, 1 AS k FROM shop.orders o",
    )
    .await;
    let cols = s.columns();
    let orders = TableRef {
        database: None,
        schema: Some("shop".into()),
        name: "orders".into(),
    };
    let origin = |i: usize| cols[i].origin.clone().map(|o| (o.table, o.column));
    assert_eq!(origin(0), Some((orders.clone(), "id".to_string())));
    assert_eq!(origin(1), Some((orders, "note".to_string())));
    assert_eq!(cols[1].name, "label");
    assert_eq!(origin(2), None);
    collect(s).await;
}

async fn describe_keys(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let conn = connect(&cfg, &sec).await;
    run(
        &conn,
        &[
            "CREATE TABLE keyed (a INT, b VARCHAR(5), c INT NOT NULL DEFAULT 7, \
             g INT AS (c * 2) VIRTUAL, PRIMARY KEY (b, a))",
            "CREATE TABLE uniq (email VARCHAR(50) UNIQUE, code INT NOT NULL, UNIQUE KEY uc (code))",
            "CREATE TABLE loose (x INT)",
        ],
    )
    .await;
    let rel = conn.relational().unwrap();
    let t = |name: &str| TableRef {
        database: None,
        schema: None,
        name: name.into(),
    };
    let keyed = rel.describe(&t("keyed")).await.unwrap();
    assert_eq!(
        keyed.table.schema.as_deref(),
        Some(cfg.values["database"].as_str())
    );
    assert_eq!(keyed.table.database, None);
    assert_eq!(keyed.primary_key, ["b", "a"]);
    assert_eq!(
        keyed.row_key(),
        Some(&["b".to_string(), "a".to_string()][..])
    );
    let cols: Vec<(&str, bool, Option<&str>, bool)> = keyed
        .columns
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                c.nullable,
                c.default.as_deref(),
                c.generated,
            )
        })
        .collect();
    assert_eq!(
        cols,
        [
            ("a", false, None, false),
            ("b", false, None, false),
            ("c", false, Some("7"), false),
            ("g", true, None, true),
        ]
    );
    assert_eq!(keyed.columns[1].type_name, "varchar(5)");

    let uniq = rel.describe(&t("uniq")).await.unwrap();
    assert!(uniq.primary_key.is_empty());
    assert_eq!(uniq.unique_keys.len(), 2);
    assert_eq!(uniq.row_key(), Some(&["code".to_string()][..]));
    assert_eq!(rel.describe(&t("loose")).await.unwrap().row_key(), None);

    let err = rel.describe(&t("missing")).await.unwrap_err();
    assert!(err.message.contains("missing"), "{err:?}");
    assert_eq!(err.kind, ErrorKind::Other, "{err:?}");
    // Another database, by name.
    let other = TableRef {
        database: None,
        schema: Some("mysql".into()),
        name: "db".into(),
    };
    assert!(!rel.describe(&other).await.unwrap().columns.is_empty());
}

async fn ddl_of_objects(server: Server) {
    let (_c, cfg, sec) = start_isolated(server).await;
    let conn = connect(&cfg, &sec).await;
    shop(&conn).await;
    let rel = conn.relational().unwrap();
    let node = |segs: &[&str], kind| Node {
        path: path(segs),
        name: segs.last().unwrap().to_string(),
        kind,
        has_children: false,
        detail: None,
    };
    let cases = [
        (
            node(&["shop", "tables", "orders"], NodeKind::Table),
            "CREATE TABLE ",
        ),
        (
            node(&["shop", "views", "v_orders"], NodeKind::View),
            " VIEW ",
        ),
        (
            node(&["shop", "functions", "f_one"], NodeKind::Function),
            " FUNCTION ",
        ),
        (
            node(&["shop", "procedures", "p_one"], NodeKind::Procedure),
            " PROCEDURE ",
        ),
        (
            node(&["shop", "triggers", "trg"], NodeKind::Trigger),
            " TRIGGER ",
        ),
    ];
    for (n, keyword) in cases {
        let ddl = rel.ddl(&n).await.unwrap_or_else(|e| panic!("{n:?}: {e:?}"));
        assert!(ddl.starts_with("CREATE"), "{ddl}");
        assert!(ddl.contains(keyword), "{keyword} not in {ddl}");
        // MariaDB keeps a trigger's statement as it was typed: names may be unquoted.
        assert!(ddl.contains(&n.name), "{} not in {ddl}", n.name);
    }
    let column = node(
        &["shop", "tables", "orders", "columns", "id"],
        NodeKind::Column,
    );
    assert_eq!(
        rel.ddl(&column).await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
}

async fn execute_with_binds_every_value_kind(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let conn = connect(&cfg, &sec).await;
    run(
        &conn,
        &["CREATE TABLE p (id INT PRIMARY KEY, n INT, b BOOL, i BIGINT, f DOUBLE, t TEXT, y BLOB)"],
    )
    .await;
    let rel = conn.relational().unwrap();
    let ph = rel.dialect().placeholder(1);
    assert_eq!(ph, "?");
    let params = [
        Value::Int(1),
        Value::Null,
        Value::Bool(true),
        Value::Int(-9_007_199_254_740_993),
        Value::Float(0.25),
        Value::Text("it's \\ 😀".into()),
        Value::Bytes(vec![0, 1, 255]),
    ];
    match rel
        .execute_with("INSERT INTO p VALUES (?, ?, ?, ?, ?, ?, ?)", &params)
        .await
        .unwrap()
    {
        Execution::Affected(n) => assert_eq!(n, 1),
        Execution::Rows(_) => panic!("rows from INSERT"),
    }
    let got = match rel
        .execute_with(
            "SELECT * FROM p WHERE id = ? AND t = ?",
            &[Value::Int(1), params[5].clone()],
        )
        .await
        .unwrap()
    {
        Execution::Rows(s) => collect(s).await,
        Execution::Affected(n) => panic!("affected {n}"),
    };
    assert_eq!(
        got,
        [[
            Value::Int(1),
            Value::Null,
            Value::Int(1),
            Value::Int(-9_007_199_254_740_993),
            Value::Float(0.25),
            Value::Text("it's \\ 😀".into()),
            Value::Bytes(vec![0, 1, 255]),
        ]]
    );
    match rel
        .execute_with(
            "UPDATE p SET n = ? WHERE id = ?",
            &[Value::Int(5), Value::Int(1)],
        )
        .await
        .unwrap()
    {
        Execution::Affected(n) => assert_eq!(n, 1),
        Execution::Rows(_) => panic!("rows from UPDATE"),
    }
    let err = rel
        .execute_with("SELECT ?", &[])
        .await
        .err()
        .expect("missing parameter");
    assert!(!err.message.is_empty());
}

async fn catalog_lists_relations(server: Server) {
    let (_c, cfg, sec) = start_isolated(server).await;
    let conn = connect(&cfg, &sec).await;
    shop(&conn).await;
    let cat = conn.relational().unwrap().catalog("shop").await.unwrap();
    assert_eq!(cat.schema, "shop");
    let rels: Vec<(&str, RelationKind, Vec<&str>)> = cat
        .relations
        .iter()
        .map(|r| {
            (
                r.name.as_str(),
                r.kind,
                r.columns.iter().map(|c| c.name.as_str()).collect(),
            )
        })
        .collect();
    assert_eq!(
        rels,
        [
            ("orders", RelationKind::Table, vec!["id", "note", "qty"]),
            ("v_orders", RelationKind::View, vec!["id", "note"]),
        ]
    );
    assert_eq!(cat.relations[0].columns[1].type_name, "varchar(20)");
    assert_eq!(
        conn.relational().unwrap().dialect().default_schemas(),
        ["test"]
    );
}

async fn connect_database_opens_other_database(server: Server) {
    let (_c, cfg, sec) = start_isolated(server).await;
    let conn = connect(&cfg, &sec).await;
    shop(&conn).await;
    let shop_conn = conn.connect_database("shop").await.unwrap();
    assert_eq!(shop_conn.database_name(), Some("shop"));
    // Unqualified names resolve in that database.
    assert_eq!(
        query(&shop_conn, "SELECT COUNT(*) FROM orders").await,
        [[Value::Int(2)]]
    );
    let again = conn.connect_database("shop").await.unwrap();
    assert!(Arc::ptr_eq(&shop_conn, &again));
    assert!(conn.connect_database("test").await.is_err());
    assert_eq!(
        conn.database_for_path(&path(&["shop", "tables", "orders"])),
        Some("shop".into())
    );
    assert_eq!(
        conn.database_for_path(&path(&["test", "tables", "t"])),
        None
    );
}

async fn warnings_become_notices(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let conn = connect(&cfg, &sec).await;
    let rel = conn.relational().unwrap();
    assert!(rel.take_notices().await.is_empty());
    // A warning at the end of a result set.
    assert_eq!(
        query(&conn, "SELECT CAST('abc' AS SIGNED)").await,
        [[Value::Int(0)]]
    );
    let notices = rel.take_notices().await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].starts_with("Warning 1292: Truncated incorrect"),
        "{notices:?}"
    );
    // A note on a statement without rows.
    run(&conn, &["DROP TABLE IF EXISTS nope"]).await;
    let notices = rel.take_notices().await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].starts_with("Note 1051: Unknown table"),
        "{notices:?}"
    );
    assert!(rel.take_notices().await.is_empty());
}

async fn quote_ident_round_trips_per_dialect(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let conn = connect(&cfg, &sec).await;
    let rel = conn.relational().unwrap();
    let d = rel.dialect();
    for name in ["Order", "my table", "select", "a\"b", "a`b"] {
        let table = TableRef {
            database: None,
            schema: Some(cfg.values["database"].clone()),
            name: name.into(),
        };
        let (t, c) = (d.qualified(&table), d.quote_ident(name));
        run(
            &conn,
            &[
                &format!("CREATE TABLE {t} ({c} INT PRIMARY KEY, other INT)"),
                &format!("INSERT INTO {t} VALUES (2, NULL), (1, 5), (3, 4)"),
            ],
        )
        .await;
        let got = query(&conn, &format!("SELECT {c} FROM {t} WHERE {c} = 1")).await;
        assert_eq!(got, [[Value::Int(1)]], "{name}");
        let info = rel.describe(&table).await.unwrap();
        assert_eq!(info.primary_key, [name], "{name}");
        // Sort and filter wrappers quote the column too; NULLs sort last.
        let base = format!("SELECT {c}, other FROM {t}");
        let sorted = query(&conn, &d.wrap_order(&base, &[("other".into(), false)])).await;
        let ids: Vec<&Value> = sorted.iter().map(|r| &r[0]).collect();
        assert_eq!(
            ids,
            [&Value::Int(3), &Value::Int(1), &Value::Int(2)],
            "{name}"
        );
        let sorted = query(&conn, &d.wrap_order(&base, &[("other".into(), true)])).await;
        let ids: Vec<&Value> = sorted.iter().map(|r| &r[0]).collect();
        assert_eq!(
            ids,
            [&Value::Int(1), &Value::Int(3), &Value::Int(2)],
            "{name}"
        );
        let filtered = query(&conn, &d.wrap_where(&base, &format!("{c} > 1"))).await;
        assert_eq!(filtered.len(), 2, "{name}");
        run(&conn, &[&format!("DROP TABLE {t}")]).await;
    }
    // String literals survive quotes and backslashes.
    let literal = d.string_literal("it's a \\ path");
    assert_eq!(
        query(&conn, &format!("SELECT {literal}")).await,
        [[Value::Text("it's a \\ path".into())]]
    );
}

async fn tls_required_encrypts(server: Server) {
    let (_c, mut cfg, sec) = start_isolated(server).await;
    cfg.values.insert("ssl_mode".into(), "required".into());
    let conn = connect(&cfg, &sec).await;
    let status = query(&conn, "SHOW SESSION STATUS LIKE 'Ssl_cipher'").await;
    let Value::Text(cipher) = &status[0][1] else {
        panic!("{status:?}")
    };
    assert!(!cipher.is_empty(), "the session is not encrypted");
}

async fn tls_required_but_unavailable(server: Server) {
    let (_c, mut cfg, sec) = start_without_tls(server).await;
    cfg.values.insert("ssl_mode".into(), "required".into());
    let started = Instant::now();
    let err = MysqlDriver::new()
        .connect(&cfg, &sec)
        .await
        .err()
        .expect("TLS is unavailable");
    assert_eq!(err.kind, ErrorKind::Connection, "{err:?}");
    assert!(err.message.contains("does not support TLS"), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(15));
    // `preferred` falls back to a plain connection.
    cfg.values.insert("ssl_mode".into(), "preferred".into());
    let conn = connect(&cfg, &sec).await;
    let status = query(&conn, "SHOW SESSION STATUS LIKE 'Ssl_cipher'").await;
    assert_eq!(status[0][1], Value::Text(String::new()));
}

/// Export `INSERT`s mean the same with and without `NO_BACKSLASH_ESCAPES`.
async fn exported_literals_read_back_in_any_sql_mode(server: Server) {
    let (_c, cfg, sec) = start(server).await;
    let conn = connect(&cfg, &sec).await;
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
    let d = conn.relational().unwrap().dialect();
    let table = TableRef {
        database: None,
        schema: Some(cfg.values["database"].clone()),
        name: "lit".into(),
    };
    let t = d.qualified(&table);
    run(
        &conn,
        &[&format!(
            "CREATE TABLE {t} (id INT PRIMARY KEY, v TEXT, b BLOB)"
        )],
    )
    .await;
    for mode in ["", "NO_BACKSLASH_ESCAPES"] {
        run(
            &conn,
            &[
                &format!("SET SESSION sql_mode = '{mode}'"),
                &format!("DELETE FROM {t}"),
            ],
        )
        .await;
        for (i, text) in texts.iter().enumerate() {
            let sql = format!(
                "INSERT INTO {t} VALUES ({i}, {}, {})",
                d.string_literal(text),
                d.bytes_literal(text.as_bytes())
            );
            run(&conn, &[&sql]).await;
        }
        let got = query(&conn, &format!("SELECT v, b FROM {t} ORDER BY id")).await;
        let expected: Vec<Vec<Value>> = texts
            .iter()
            .map(|s| {
                vec![
                    Value::Text(s.to_string()),
                    Value::Bytes(s.as_bytes().to_vec()),
                ]
            })
            .collect();
        assert_eq!(got, expected, "sql_mode = '{mode}'");
    }
}

#[tokio::test]
async fn mysql_sql_mode_applied() {
    sqlr_testkit::skip_container_tests!("MYSQL");
    let (_container, mut cfg, secrets) = start(Server::MySql).await;
    cfg.values
        .insert("property.sql_mode".into(), "ANSI_QUOTES".into());
    cfg.values
        .insert("property.wait_timeout".into(), "1234".into());
    let conn = connect(&cfg, &secrets).await;
    let rows = query(&conn, "SELECT @@SESSION.sql_mode, @@SESSION.wait_timeout").await;
    assert_eq!(rows[0][0], Value::Text("ANSI_QUOTES".into()));
    // `wait_timeout` is an unsigned bigint; unsigned 64-bit values stay decimal text.
    assert_eq!(rows[0][1], Value::Text("1234".into()));
}
