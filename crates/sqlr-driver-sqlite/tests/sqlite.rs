use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlr_core::{
    Cell, ColumnKind, ColumnOrigin, Connection, ConnectionConfig, DisplayHint, Driver, ErrorKind,
    Execution, Node, NodeKind, NodePath, RowStream, Secrets, TableRef, Value,
};
use sqlr_driver_sqlite::SqliteDriver;

fn config(path: &Path, read_only: bool, create: bool) -> ConnectionConfig {
    ConnectionConfig {
        id: "t".into(),
        name: "test".into(),
        driver: "sqlite".into(),
        group: None,
        color: None,
        values: [
            ("path", path.display().to_string()),
            ("read_only", read_only.to_string()),
            ("create_if_missing", create.to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        profile: None,
        save_secrets: true,
    }
}

async fn open(path: &Path) -> Arc<dyn Connection> {
    SqliteDriver::new()
        .connect(&config(path, false, true), &Secrets::default())
        .await
        .unwrap()
}

async fn temp_db() -> (tempfile::TempDir, Arc<dyn Connection>) {
    let dir = tempfile::tempdir().unwrap();
    let conn = open(&dir.path().join("test.db")).await;
    (dir, conn)
}

async fn exec(conn: &Arc<dyn Connection>, sql: &str) -> Execution {
    conn.relational()
        .unwrap()
        .execute(sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"))
}

async fn affected(conn: &Arc<dyn Connection>, sql: &str) -> u64 {
    match exec(conn, sql).await {
        Execution::Affected(n) => n,
        Execution::Rows(_) => panic!("{sql}: expected Affected, got rows"),
    }
}

async fn rows(conn: &Arc<dyn Connection>, sql: &str) -> Box<dyn RowStream> {
    match exec(conn, sql).await {
        Execution::Rows(s) => s,
        Execution::Affected(n) => panic!("{sql}: expected rows, got Affected({n})"),
    }
}

/// Every value of the result, row by row.
async fn values(stream: &mut Box<dyn RowStream>) -> Vec<Vec<Value>> {
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
    out
}

async fn query(conn: &Arc<dyn Connection>, sql: &str) -> Vec<Vec<Value>> {
    let mut s = rows(conn, sql).await;
    let v = values(&mut s).await;
    s.close().await.unwrap();
    v
}

fn path(segs: &[&str]) -> NodePath {
    NodePath(segs.iter().map(|s| s.to_string()).collect())
}

fn names(nodes: &[Node]) -> Vec<&str> {
    nodes.iter().map(|n| n.name.as_str()).collect()
}

#[tokio::test]
async fn connect_and_ping() {
    let (_dir, conn) = temp_db().await;
    let info = conn.ping().await.unwrap();
    assert_eq!(info.product, "SQLite");
    assert!(info.version.starts_with('3'), "{}", info.version);
}

#[tokio::test]
async fn missing_file_without_create_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(&dir.path().join("missing.db"), false, false);
    let err = match SqliteDriver::new().connect(&cfg, &Secrets::default()).await {
        Ok(_) => panic!("opened a missing file"),
        Err(e) => e,
    };
    assert_eq!(err.kind, ErrorKind::Connection);
}

#[tokio::test]
async fn read_only_refuses_writes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ro.db");
    let rw = open(&file).await;
    affected(&rw, "CREATE TABLE t (x INTEGER)").await;
    affected(&rw, "INSERT INTO t VALUES (1)").await;
    drop(rw);

    let ro = SqliteDriver::new()
        .connect(&config(&file, true, false), &Secrets::default())
        .await
        .unwrap();
    assert_eq!(query(&ro, "SELECT x FROM t").await, [[Value::Int(1)]]);
    let err = match ro
        .relational()
        .unwrap()
        .execute("INSERT INTO t VALUES (2)")
        .await
    {
        Ok(_) => panic!("write allowed on a read-only connection"),
        Err(e) => e,
    };
    assert_eq!(err.kind, ErrorKind::Permission, "{err:?}");
}

/// The session read-only switch (`read_only_sql`) is enforced by SQLite itself, without the
/// engine's classifier.
#[tokio::test]
async fn read_only_enforced_by_server() {
    let (_dir, conn) = temp_db().await;
    affected(&conn, "CREATE TABLE t (x INTEGER)").await;
    let rel = conn.relational().unwrap();
    let on = rel.dialect().read_only_sql(true).unwrap();
    exec(&conn, &on).await;
    let err = match rel.execute("INSERT INTO t VALUES (1)").await {
        Ok(_) => panic!("write allowed after {on}"),
        Err(e) => e,
    };
    assert!(err.message.contains("readonly"), "{err:?}");
    let off = rel.dialect().read_only_sql(false).unwrap();
    exec(&conn, &off).await;
    affected(&conn, "INSERT INTO t VALUES (1)").await;
}

async fn setup_objects(conn: &Arc<dyn Connection>) {
    for sql in [
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT)",
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, total DECIMAL(10,2))",
        "CREATE VIEW v_users AS SELECT id, name FROM users",
        "CREATE INDEX orders_user ON orders (user_id)",
        "CREATE TRIGGER users_ai AFTER INSERT ON users BEGIN SELECT 1; END",
    ] {
        affected(conn, sql).await;
    }
}

#[tokio::test]
async fn browse_tree_shape() {
    let (_dir, conn) = temp_db().await;
    setup_objects(&conn).await;

    let schemas = conn.browse(&NodePath::root()).await.unwrap();
    assert_eq!(names(&schemas), ["main"]);
    assert_eq!(schemas[0].kind, NodeKind::Schema);

    let folders = conn.browse(&path(&["main"])).await.unwrap();
    assert_eq!(names(&folders), ["tables", "views", "indexes", "triggers"]);

    let tables = conn.browse(&path(&["main", "tables"])).await.unwrap();
    assert_eq!(names(&tables), ["orders", "users"]);
    assert!(
        tables
            .iter()
            .all(|n| n.kind == NodeKind::Table && n.has_children)
    );

    let views = conn.browse(&path(&["main", "views"])).await.unwrap();
    assert_eq!(names(&views), ["v_users"]);
    assert_eq!(views[0].kind, NodeKind::View);

    let indexes = conn.browse(&path(&["main", "indexes"])).await.unwrap();
    assert_eq!(names(&indexes), ["orders_user"]);
    assert_eq!(indexes[0].kind, NodeKind::Index);

    let triggers = conn.browse(&path(&["main", "triggers"])).await.unwrap();
    assert_eq!(names(&triggers), ["users_ai"]);
    assert_eq!(triggers[0].kind, NodeKind::Trigger);

    let columns = conn
        .browse(&path(&["main", "tables", "users"]))
        .await
        .unwrap();
    assert_eq!(names(&columns), ["id", "name", "email"]);
    let details: Vec<&str> = columns
        .iter()
        .map(|c| c.detail.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(details, ["INTEGER PK", "TEXT NOT NULL", "TEXT"]);
    let view_columns = conn
        .browse(&path(&["main", "views", "v_users"]))
        .await
        .unwrap();
    assert_eq!(names(&view_columns), ["id", "name"]);

    let err = conn.browse(&path(&["main", "nope"])).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);

    // `temp` shows once it holds objects.
    affected(&conn, "CREATE TEMP TABLE scratch (x)").await;
    let schemas = conn.browse(&NodePath::root()).await.unwrap();
    assert_eq!(names(&schemas), ["main", "temp"]);
}

#[tokio::test]
async fn attached_database_browse() {
    let (dir, conn) = temp_db().await;
    let other = dir.path().join("other.db");
    let attach = format!(
        "ATTACH DATABASE '{}' AS other",
        other.display().to_string().replace('\'', "''")
    );
    affected(&conn, &attach).await;
    affected(&conn, "CREATE TABLE other.things (id INTEGER PRIMARY KEY)").await;
    let schemas = conn.browse(&NodePath::root()).await.unwrap();
    assert_eq!(names(&schemas), ["main", "other"]);
    let tables = conn.browse(&path(&["other", "tables"])).await.unwrap();
    assert_eq!(names(&tables), ["things"]);
    let dialect = conn.relational().unwrap().dialect();
    let sql = dialect.select_all(&tables[0], Some(10)).unwrap();
    assert_eq!(sql, "SELECT * FROM \"other\".\"things\" LIMIT 10");
    assert!(query(&conn, &sql).await.is_empty());
}

#[tokio::test]
async fn streams_100k_rows_in_chunks() {
    let (_dir, conn) = temp_db().await;
    let mut s = rows(
        &conn,
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 100000) \
         SELECT n FROM c",
    )
    .await;
    assert_eq!(s.columns()[0].kind, ColumnKind::Mixed);
    let mut total = 0usize;
    let mut chunks = 0;
    while let Some(chunk) = s.next_chunk(5000).await.unwrap() {
        assert!(chunk.len() <= 5000);
        for r in 0..chunk.len() {
            assert_eq!(chunk.cell(r, 0), Cell::Int(total as i64 + 1));
            total += 1;
        }
        chunks += 1;
    }
    assert_eq!(total, 100_000);
    assert_eq!(chunks, 20);
    assert!(s.next_chunk(5000).await.unwrap().is_none());
    s.close().await.unwrap();
    s.close().await.unwrap();
}

#[tokio::test]
async fn cancel_recursive_cte_within_1s() {
    let (_dir, conn) = temp_db().await;
    let cancel = conn.cancel_handle();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel().await.unwrap();
    });
    let started = Instant::now();
    let result = async {
        let rel = conn.relational().unwrap();
        let exec = rel
            .execute(
                "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c) \
                 SELECT count(*) FROM c",
            )
            .await?;
        match exec {
            Execution::Rows(mut s) => s.next_chunk(10).await.map(|_| ()),
            Execution::Affected(_) => Ok(()),
        }
    }
    .await;
    let err = result.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled, "{err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    canceller.await.unwrap();
    // The connection stays usable.
    assert_eq!(query(&conn, "SELECT 1").await, [[Value::Int(1)]]);
}

#[tokio::test]
async fn typed_values_by_storage_class() {
    let (_dir, conn) = temp_db().await;
    affected(
        &conn,
        "CREATE TABLE t (i INTEGER, r REAL, s TEXT, b BLOB, d DECIMAL(10,2), dt DATETIME, \
         j JSON, flag BOOLEAN, x)",
    )
    .await;
    affected(
        &conn,
        "INSERT INTO t VALUES (7, 1.5, 'è', x'0102', 12.5, '2024-01-02 03:04:05', '{\"a\":1}', 1, 'any'), \
         (NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)",
    )
    .await;
    let mut s = rows(&conn, "SELECT * FROM t").await;
    let cols = s.columns();
    assert!(cols.iter().all(|c| c.kind == ColumnKind::Mixed));
    let hints: Vec<DisplayHint> = cols.iter().map(|c| c.hint).collect();
    use DisplayHint as H;
    assert_eq!(
        hints,
        [
            H::Number,
            H::Number,
            H::Plain,
            H::Binary,
            H::Decimal,
            H::Temporal,
            H::Json,
            H::Plain,
            H::Plain
        ]
    );
    assert_eq!(cols[4].type_name, "DECIMAL(10,2)");
    assert_eq!(cols[8].type_name, "");
    let v = values(&mut s).await;
    assert_eq!(
        v[0],
        [
            Value::Int(7),
            Value::Float(1.5),
            Value::Text("è".into()),
            Value::Bytes(vec![1, 2]),
            Value::Float(12.5),
            Value::Text("2024-01-02 03:04:05".into()),
            Value::Text("{\"a\":1}".into()),
            Value::Int(1),
            Value::Text("any".into()),
        ]
    );
    assert!(v[1].iter().all(Value::is_null));

    let mut s = rows(&conn, "SELECT 1, 2.5, 'a', x'ff', NULL").await;
    assert!(s.columns().iter().all(|c| c.kind == ColumnKind::Mixed));
    assert_eq!(
        values(&mut s).await,
        [[
            Value::Int(1),
            Value::Float(2.5),
            Value::Text("a".into()),
            Value::Bytes(vec![0xff]),
            Value::Null
        ]]
    );
}

#[tokio::test]
async fn mixed_storage_classes_come_back_as_stored() {
    let (_dir, conn) = temp_db().await;
    affected(&conn, "CREATE TABLE m (a INTEGER, t TEXT, u)").await;
    affected(
        &conn,
        "INSERT INTO m VALUES (1, 'x', 1), ('abc', x'ff00', 'two'), (1.5, NULL, 2.5), \
         (NULL, 'y', x'01')",
    )
    .await;
    assert_eq!(
        query(&conn, "SELECT a, t, u FROM m").await,
        [
            [Value::Int(1), Value::Text("x".into()), Value::Int(1)],
            [
                Value::Text("abc".into()),
                Value::Bytes(vec![0xff, 0]),
                Value::Text("two".into())
            ],
            [Value::Float(1.5), Value::Null, Value::Float(2.5)],
            [Value::Null, Value::Text("y".into()), Value::Bytes(vec![1])],
        ]
    );
    assert!(conn.relational().unwrap().take_notices().await.is_empty());
}

#[tokio::test]
async fn origins_on_select_star() {
    let (_dir, conn) = temp_db().await;
    setup_objects(&conn).await;
    let s = rows(&conn, "SELECT *, 1 AS one FROM users").await;
    let origins: Vec<Option<ColumnOrigin>> = s.columns().iter().map(|c| c.origin.clone()).collect();
    let table = TableRef {
        database: None,
        schema: Some("main".into()),
        name: "users".into(),
    };
    let origin = |c: &str| {
        Some(ColumnOrigin {
            table: table.clone(),
            column: c.into(),
        })
    };
    assert_eq!(
        origins,
        [origin("id"), origin("name"), origin("email"), None]
    );
}

#[tokio::test]
async fn describe_keys() {
    let (_dir, conn) = temp_db().await;
    affected(
        &conn,
        "CREATE TABLE k (a INTEGER, b TEXT, c TEXT NOT NULL DEFAULT 'x', \
         g INTEGER GENERATED ALWAYS AS (a * 2), PRIMARY KEY (b, a))",
    )
    .await;
    affected(
        &conn,
        "CREATE TABLE u (id INTEGER, email TEXT UNIQUE, code TEXT NOT NULL UNIQUE)",
    )
    .await;
    let rel = conn.relational().unwrap();
    let table = |name: &str| TableRef {
        database: None,
        schema: None,
        name: name.into(),
    };
    let k = rel.describe(&table("k")).await.unwrap();
    assert_eq!(k.primary_key, ["b", "a"]);
    let cols: Vec<(&str, bool, bool)> = k
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.nullable, c.generated))
        .collect();
    assert_eq!(
        cols,
        [
            ("a", true, false),
            ("b", true, false),
            ("c", false, false),
            ("g", true, true)
        ]
    );
    assert_eq!(k.columns[2].default.as_deref(), Some("'x'"));
    assert_eq!(k.row_key(), Some(&["b".to_string(), "a".to_string()][..]));

    let u = rel.describe(&table("u")).await.unwrap();
    assert!(u.primary_key.is_empty());
    let mut keys = u.unique_keys.clone();
    keys.sort();
    assert_eq!(keys, [vec!["code".to_string()], vec!["email".to_string()]]);
    assert_eq!(u.row_key(), Some(&["code".to_string()][..]));

    let err = rel.describe(&table("missing")).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Other, "{err:?}");
}

#[tokio::test]
async fn ddl_of_objects() {
    let (_dir, conn) = temp_db().await;
    setup_objects(&conn).await;
    let rel = conn.relational().unwrap();
    let tables = conn.browse(&path(&["main", "tables"])).await.unwrap();
    let users = tables.iter().find(|n| n.name == "users").unwrap();
    let ddl = rel.ddl(users).await.unwrap();
    assert!(ddl.starts_with("CREATE TABLE users ("), "{ddl}");
    assert!(ddl.contains("CREATE TRIGGER users_ai"), "{ddl}");
    let orders = tables.iter().find(|n| n.name == "orders").unwrap();
    assert!(
        rel.ddl(orders)
            .await
            .unwrap()
            .contains("CREATE INDEX orders_user ON orders (user_id);")
    );
    let views = conn.browse(&path(&["main", "views"])).await.unwrap();
    assert_eq!(
        rel.ddl(&views[0]).await.unwrap(),
        "CREATE VIEW v_users AS SELECT id, name FROM users;"
    );
    let columns = conn
        .browse(&path(&["main", "tables", "users"]))
        .await
        .unwrap();
    assert_eq!(
        rel.ddl(&columns[0]).await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
}

#[tokio::test]
async fn execute_with_binds_every_value_kind() {
    let (_dir, conn) = temp_db().await;
    affected(&conn, "CREATE TABLE p (a, b, c, d, e, f)").await;
    let rel = conn.relational().unwrap();
    let params = [
        Value::Null,
        Value::Bool(true),
        Value::Int(-9),
        Value::Float(0.25),
        Value::Text("it's 😀".into()),
        Value::Bytes(vec![0, 1, 255]),
    ];
    let placeholders: Vec<String> = (1..=6).map(|i| rel.dialect().placeholder(i)).collect();
    let sql = format!("INSERT INTO p VALUES ({})", placeholders.join(", "));
    match rel.execute_with(&sql, &params).await.unwrap() {
        Execution::Affected(n) => assert_eq!(n, 1),
        Execution::Rows(_) => panic!("expected Affected"),
    }
    assert_eq!(
        query(&conn, "SELECT * FROM p").await,
        [[
            Value::Null,
            Value::Int(1),
            Value::Int(-9),
            Value::Float(0.25),
            Value::Text("it's 😀".into()),
            Value::Bytes(vec![0, 1, 255]),
        ]]
    );
    let mut s = match rel
        .execute_with("SELECT e FROM p WHERE c = ?", &[Value::Int(-9)])
        .await
        .unwrap()
    {
        Execution::Rows(s) => s,
        Execution::Affected(_) => panic!("expected rows"),
    };
    assert_eq!(values(&mut s).await, [[Value::Text("it's 😀".into())]]);
    let err = match rel.execute_with("SELECT ?", &[]).await {
        Ok(_) => panic!("missing parameter accepted"),
        Err(e) => e,
    };
    assert_eq!(err.kind, ErrorKind::Other, "{err:?}");
}

#[tokio::test]
async fn catalog_lists_relations_and_columns() {
    let (_dir, conn) = temp_db().await;
    setup_objects(&conn).await;
    let cat = conn.relational().unwrap().catalog("main").await.unwrap();
    let rels: Vec<(&str, usize)> = cat
        .relations
        .iter()
        .map(|r| (r.name.as_str(), r.columns.len()))
        .collect();
    assert_eq!(rels, [("orders", 3), ("users", 3), ("v_users", 2)]);
    assert_eq!(cat.relations[1].columns[1].type_name, "TEXT");
}

#[tokio::test]
async fn quote_ident_round_trips_per_dialect() {
    let (_dir, conn) = temp_db().await;
    let rel = conn.relational().unwrap();
    let d = rel.dialect();
    for name in ["Order", "my table", "select", "a\"b", "a`b", "a]b"] {
        let table = TableRef {
            database: None,
            schema: Some("main".into()),
            name: name.into(),
        };
        let q = d.qualified(&table);
        let col = d.quote_ident(name);
        affected(&conn, &format!("CREATE TABLE {q} ({col} INTEGER)")).await;
        affected(&conn, &format!("INSERT INTO {q} ({col}) VALUES (1)")).await;
        let s = rows(&conn, &format!("SELECT {col} FROM {q}")).await;
        assert_eq!(s.columns()[0].name, name);
        drop(s);
        let tables = conn.browse(&path(&["main", "tables"])).await.unwrap();
        let node = tables.iter().find(|n| n.name == name).unwrap();
        let sql = d.select_all(node, None).unwrap();
        assert_eq!(query(&conn, &sql).await, [[Value::Int(1)]], "{name}");
        let info = rel.describe(&table).await.unwrap();
        assert_eq!(info.columns[0].name, name);
    }
}

#[tokio::test]
async fn new_execute_ends_the_open_stream() {
    let (_dir, conn) = temp_db().await;
    let mut first = rows(
        &conn,
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 100) SELECT n FROM c",
    )
    .await;
    assert_eq!(first.next_chunk(10).await.unwrap().unwrap().len(), 10);
    assert_eq!(query(&conn, "SELECT 2").await, [[Value::Int(2)]]);
    // The cut-off result must not look complete.
    let err = first.next_chunk(10).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled, "{err:?}");
    assert!(err.message.contains("newer statement"), "{err:?}");
    first.close().await.unwrap();

    // A result read to its end stays complete after the next statement.
    let mut done = rows(&conn, "SELECT 1").await;
    assert_eq!(done.next_chunk(10).await.unwrap().unwrap().len(), 1);
    assert!(done.next_chunk(10).await.unwrap().is_none());
    assert_eq!(query(&conn, "SELECT 3").await, [[Value::Int(3)]]);
    assert!(done.next_chunk(10).await.unwrap().is_none());
}

#[tokio::test]
async fn cancel_while_idle_is_ignored() {
    let (_dir, conn) = temp_db().await;
    let mut s = rows(
        &conn,
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 100) SELECT n FROM c",
    )
    .await;
    assert_eq!(s.next_chunk(10).await.unwrap().unwrap().len(), 10);
    // Nothing runs between fetches: a cancel now targets no statement.
    conn.cancel_handle().cancel().await.unwrap();
    assert_eq!(conn.browse(&NodePath::root()).await.unwrap().len(), 1);
    assert_eq!(s.next_chunk(10).await.unwrap().unwrap().len(), 10);
    s.close().await.unwrap();
    conn.cancel_handle().cancel().await.unwrap();
    assert_eq!(query(&conn, "SELECT 1").await, [[Value::Int(1)]]);
}

#[tokio::test]
async fn dialect_sql_runs() {
    let (_dir, conn) = temp_db().await;
    affected(&conn, "CREATE TABLE t (a INTEGER, b TEXT)").await;
    affected(
        &conn,
        "INSERT INTO t VALUES (2, 'x'), (NULL, 'y'), (1, 'z')",
    )
    .await;
    let d = conn.relational().unwrap().dialect();
    let sorted = d.wrap_order("SELECT * FROM t -- trailing", &[("a".into(), true)]);
    let firsts: Vec<Value> = query(&conn, &sorted)
        .await
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    assert_eq!(firsts, [Value::Int(2), Value::Int(1), Value::Null]);
    let filtered = d.wrap_where("SELECT * FROM t;", "b = 'y'");
    assert_eq!(query(&conn, &filtered).await.len(), 1);

    affected(&conn, d.begin_sql()).await;
    affected(&conn, &d.savepoint_sql("s 1")).await;
    affected(&conn, "INSERT INTO t VALUES (9, 'w')").await;
    affected(&conn, &d.rollback_to_sql("s 1")).await;
    affected(&conn, "COMMIT").await;
    assert_eq!(query(&conn, "SELECT * FROM t").await.len(), 3);

    affected(&conn, &d.read_only_sql(true).unwrap()).await;
    let err = match conn
        .relational()
        .unwrap()
        .execute("INSERT INTO t VALUES (3, 'q')")
        .await
    {
        Ok(_) => panic!("query_only allowed a write"),
        Err(e) => e,
    };
    assert_eq!(err.kind, ErrorKind::Permission, "{err:?}");
    affected(&conn, &d.read_only_sql(false).unwrap()).await;
    affected(&conn, "INSERT INTO t VALUES (3, 'q')").await;
}

#[tokio::test]
async fn syntax_error_has_position() {
    let (_dir, conn) = temp_db().await;
    let err = match conn.relational().unwrap().execute("SELECT 1 FROMM t").await {
        Ok(_) => panic!("bad SQL accepted"),
        Err(e) => e,
    };
    assert_eq!(err.kind, ErrorKind::Syntax, "{err:?}");
    assert!(err.position.is_some(), "{err:?}");
}

#[tokio::test]
async fn sqlite_pragmas_applied() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(&dir.path().join("props.db"), false, true);
    cfg.values
        .insert("property.foreign_keys".into(), "true".into());
    cfg.values
        .insert("property.busy_timeout".into(), "1234".into());
    let conn = SqliteDriver::new()
        .connect(&cfg, &Secrets::default())
        .await
        .unwrap();
    assert_eq!(
        query(&conn, "PRAGMA foreign_keys").await,
        vec![vec![Value::Int(1)]]
    );
    assert_eq!(
        query(&conn, "PRAGMA busy_timeout").await,
        vec![vec![Value::Int(1234)]]
    );
}
