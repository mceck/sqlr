//! NoSQL transfer endpoints: collections and indices as sources and
//! targets, Redis keys as a source. The fixed services come from the test compose file.

mod support;

use std::time::Duration;

use sqlr_core::{
    ConnectionConfig, CsvOptions, DocTarget, DocValue, Endpoint, Secrets, TableRef, TextFormat,
    TransferSpec, Value,
};
use sqlr_engine::{
    DriverRegistry, Engine, ImportOptions, JobId, JobOutcome, QueryId, QueryStatus, SessionKey,
    SessionStatus,
};
use sqlr_testkit::TestLock;
use support::{
    TIMEOUT, config, dev, endpoint, no_repaint, reset_elastic, reset_mongo, reset_redis, wait_until,
};

fn engine() -> Engine {
    let mut drivers = DriverRegistry::default();
    drivers.register(sqlr_driver_mongo::MongoDriver::new());
    drivers.register(sqlr_driver_postgres::PostgresDriver::new());
    drivers.register(sqlr_driver_elastic::ElasticDriver::new());
    drivers.register(sqlr_driver_redis::RedisDriver::new());
    let engine = Engine::new(drivers, no_repaint());
    engine.set_connect_timeout(60);
    engine
}

/// Connects `cfg`, retrying while the server finishes starting (up to `limit`).
fn connect(engine: &Engine, cfg: ConnectionConfig, secrets: Secrets, limit: Duration) {
    let id = cfg.id.clone();
    let deadline = std::time::Instant::now() + limit;
    loop {
        engine.connect(cfg.clone(), secrets.clone(), dev());
        wait_until(Duration::from_secs(70), || {
            !matches!(engine.session_status(&id), Some(SessionStatus::Connecting))
        });
        if matches!(
            engine.session_status(&id),
            Some(SessionStatus::Connected(_))
        ) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{id} never connected");
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn values(pairs: &[(&str, String)]) -> std::collections::BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn job(engine: &Engine, id: JobId) -> JobOutcome {
    assert!(
        wait_until(Duration::from_secs(120), || engine
            .job(id)
            .is_some_and(|j| j.outcome.is_some())),
        "the job never finished"
    );
    engine.job(id).unwrap().outcome.unwrap()
}

/// Runs a document statement on `conn`'s explorer session; panics on failure.
fn doc_run(engine: &Engine, conn: &str, text: &str) -> QueryId {
    let q = engine.run_doc(&SessionKey::meta(conn), None, text.into(), 1_000);
    assert!(wait_until(TIMEOUT * 12, || engine
        .doc_result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
    let status = engine.doc_result(q).unwrap().read().status.clone();
    assert!(
        !matches!(status, QueryStatus::Failed(_)),
        "{text}: {status:?}"
    );
    q
}

fn docs(engine: &Engine, q: QueryId) -> Vec<DocValue> {
    let r = engine.doc_result(q).unwrap();
    let r = r.read();
    (0..r.len()).filter_map(|i| r.doc(i)).collect()
}

/// Rows of a SQL query on `conn`.
fn sql_rows(engine: &Engine, conn: &str, sql: &str) -> Vec<Vec<Value>> {
    let q = engine.execute(conn, sql.into(), 1_000);
    assert!(wait_until(TIMEOUT * 4, || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
    let r = engine.result(q).unwrap();
    let r = r.read();
    assert!(
        matches!(r.status, QueryStatus::Ready { .. }),
        "{sql}: {:?}",
        r.status
    );
    (0..r.rows.row_count())
        .map(|row| {
            (0..r.rows.columns().len())
                .map(|c| Value::from_cell(r.rows.cell(row, c).unwrap()))
                .collect()
        })
        .collect()
}

struct Mongo {
    _lock: TestLock,
    cfg: ConnectionConfig,
}

fn mongo(rt: &tokio::runtime::Runtime) -> Mongo {
    let (host, port) = endpoint("MONGO");
    let lock = TestLock::acquire("mongo");
    rt.block_on(reset_mongo(&host, port));
    Mongo {
        _lock: lock,
        cfg: ConnectionConfig {
            values: values(&[
                ("host", host),
                ("port", port.to_string()),
                ("database", "test".into()),
            ]),
            ..config("m", "mongodb")
        },
    }
}

fn people() -> DocTarget {
    DocTarget {
        database: None,
        collection: "people".into(),
    }
}

#[test]
fn mongo_to_pg_flattened() {
    sqlr_testkit::skip_container_tests!();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let m = mongo(&rt);
    let (host, port) = endpoint("POSTGRES");
    let engine = engine();
    connect(
        &engine,
        m.cfg.clone(),
        Secrets::default(),
        Duration::from_secs(120),
    );
    let pg = ConnectionConfig {
        values: values(&[
            ("host", host),
            ("port", port.to_string()),
            ("database", "postgres".into()),
            ("user", "postgres".into()),
        ]),
        ..config("p", "postgres")
    };
    let secrets = Secrets([("password".to_string(), "postgres".to_string())].into());
    connect(&engine, pg, secrets, Duration::from_secs(60));
    sql_rows(&engine, "p", "DROP TABLE IF EXISTS public.people");
    sql_rows(
        &engine,
        "p",
        "CREATE TABLE people (_id text, name text, age bigint, address jsonb, tags text, \
         email text) ",
    );
    doc_run(
        &engine,
        "m",
        "db.people.insertMany([\
         {_id: ObjectId(\"65f0a1b2c3d4e5f601234567\"), name: \"ann\", age: 30, \
          address: {city: \"Rome\", zip: \"00100\"}, tags: [\"a\", \"b\"]}, \
         {_id: ObjectId(\"65f0a1b2c3d4e5f601234568\"), name: \"bob\", email: \"b@x.io\"}])",
    );
    let id = engine.transfer(TransferSpec {
        source: Endpoint::Collection {
            conn: "m".into(),
            target: people(),
            query: Some("db.people.find({}).sort({name: 1})".into()),
        },
        target: Endpoint::Table {
            conn: "p".into(),
            table: TableRef {
                database: None,
                schema: Some("public".into()),
                name: "people".into(),
            },
        },
        mapping: Vec::new(),
        options: ImportOptions::default(),
    });
    match job(&engine, id) {
        JobOutcome::Imported {
            read: 2,
            written: 2,
            rejected,
        } => assert!(rejected.is_empty()),
        other => panic!("{other:?}"),
    }
    let rows = sql_rows(
        &engine,
        "p",
        "SELECT _id, name, age, address->>'city', tags, email FROM people ORDER BY name",
    );
    assert_eq!(
        rows,
        [
            vec![
                Value::Text("65f0a1b2c3d4e5f601234567".into()),
                Value::Text("ann".into()),
                Value::Int(30),
                Value::Text("Rome".into()),
                Value::Text("[\"a\", \"b\"]".into()),
                Value::Null,
            ],
            vec![
                Value::Text("65f0a1b2c3d4e5f601234568".into()),
                Value::Text("bob".into()),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Text("b@x.io".into()),
            ],
        ]
    );
}

#[test]
fn csv_to_mongo_documents() {
    sqlr_testkit::skip_container_tests!();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let m = mongo(&rt);
    let engine = engine();
    connect(
        &engine,
        m.cfg.clone(),
        Secrets::default(),
        Duration::from_secs(120),
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("people.csv");
    std::fs::write(
        &path,
        "name,age,score,zip,active,skipped\nann,30,1.5,01234,true,x\nbob,,2,99,false,y\n",
    )
    .unwrap();
    let id = engine.transfer(TransferSpec {
        source: Endpoint::File {
            path,
            format: TextFormat::Csv(CsvOptions::default()),
        },
        target: Endpoint::Collection {
            conn: "m".into(),
            target: people(),
            query: None,
        },
        mapping: vec![
            (0, Some("name".into())).into(),
            (1, Some("age".into())).into(),
            (2, Some("score".into())).into(),
            (3, Some("zip".into())).into(),
            (4, Some("active".into())).into(),
            (5, None).into(),
        ],
        options: ImportOptions::default(),
    });
    assert_eq!(
        job(&engine, id),
        JobOutcome::Imported {
            read: 2,
            written: 2,
            rejected: Vec::new()
        }
    );
    let q = doc_run(&engine, "m", "db.people.find({}, {_id: 0}).sort({name: 1})");
    let doc = |fields: Vec<(&str, DocValue)>| {
        DocValue::Document(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    };
    assert_eq!(
        docs(&engine, q),
        [
            doc(vec![
                ("name", DocValue::String("ann".into())),
                ("age", DocValue::Int32(30)),
                ("score", DocValue::Double(1.5)),
                ("zip", DocValue::String("01234".into())),
                ("active", DocValue::Bool(true)),
            ]),
            doc(vec![
                ("name", DocValue::String("bob".into())),
                ("age", DocValue::Null),
                ("score", DocValue::Int32(2)),
                ("zip", DocValue::Int32(99)),
                ("active", DocValue::Bool(false)),
            ]),
        ]
    );
}

#[test]
fn es_to_jsonl() {
    sqlr_testkit::skip_container_tests!();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let (host, port) = endpoint("ELASTIC");
    let _lock = TestLock::acquire("elasticsearch");
    rt.block_on(reset_elastic(&host, port));
    let engine = engine();
    let cfg = ConnectionConfig {
        values: values(&[("host", host), ("port", port.to_string())]),
        ..config("e", "elasticsearch")
    };
    connect(&engine, cfg, Secrets::default(), Duration::from_secs(240));
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let q = engine.run_doc(
            &SessionKey::meta("e"),
            None,
            "GET _cluster/health?wait_for_status=yellow&timeout=60s".into(),
            10,
        );
        wait_until(Duration::from_secs(70), || {
            engine
                .doc_result(q)
                .is_some_and(|r| r.read().status != QueryStatus::Running)
        });
        if matches!(
            engine.doc_result(q).unwrap().read().status,
            QueryStatus::Ready { .. }
        ) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the cluster never got ready"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
    let mut bulk = String::from("POST logs/_bulk?refresh=true\n");
    for i in 0..1_234 {
        bulk.push_str(&format!(
            "{{\"index\": {{\"_id\": \"{i}\"}}}}\n{{\"n\": {i}, \"ctx\": {{\"host\": \"h{}\"}}}}\n",
            i % 4
        ));
    }
    doc_run(&engine, "e", &bulk);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("logs.jsonl");
    let id = engine.transfer(TransferSpec {
        source: Endpoint::Collection {
            conn: "e".into(),
            target: DocTarget {
                database: None,
                collection: "logs".into(),
            },
            query: None,
        },
        target: Endpoint::File {
            path: path.clone(),
            format: TextFormat::JsonLines,
        },
        mapping: Vec::new(),
        options: ImportOptions::default(),
    });
    match job(&engine, id) {
        JobOutcome::Exported { rows: 1_234, .. } => {}
        other => panic!("{other:?}"),
    }
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1_234);
    let mut seen: Vec<i64> = lines.iter().map(|l| l["n"].as_i64().unwrap()).collect();
    seen.sort_unstable();
    assert_eq!(seen, (0..1_234).collect::<Vec<_>>(), "every document, once");
    assert_eq!(lines[0]["_index"], "logs");
    assert!(
        lines[0]["ctx"]["host"].is_string(),
        "nested values stay nested"
    );
}

#[test]
fn redis_keys_to_csv() {
    sqlr_testkit::skip_container_tests!();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let (host, port) = endpoint("REDIS");
    let _lock = TestLock::acquire("redis");
    rt.block_on(reset_redis(&host, port));
    let engine = engine();
    let cfg = ConnectionConfig {
        values: values(&[
            ("host", host),
            ("port", port.to_string()),
            ("database", "0".into()),
        ]),
        ..config("r", "redis")
    };
    connect(&engine, cfg, Secrets::default(), Duration::from_secs(60));
    for line in [
        "SET user:1 \"hello world\"",
        "HSET user:2 name ann age 30",
        "RPUSH user:3 a b",
        "SADD user:4 x",
        "ZADD user:5 1.5 m",
        "XADD user:6 1-1 f v",
        "SET other k",
        "PEXPIRE user:1 100000000",
    ] {
        engine.kv_command("r", 1, line);
    }
    assert!(wait_until(TIMEOUT, || {
        let c = engine.kv_console("r", 1);
        c.output.len() == 8 && !c.running
    }));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.csv");
    let id = engine.transfer(TransferSpec {
        source: Endpoint::RedisKeys {
            conn: "r".into(),
            db: 0,
            pattern: "user:*".into(),
        },
        target: Endpoint::File {
            path: path.clone(),
            format: TextFormat::Csv(CsvOptions::default()),
        },
        mapping: Vec::new(),
        options: ImportOptions::default(),
    });
    match job(&engine, id) {
        JobOutcome::Exported { rows: 6, .. } => {}
        other => panic!("{other:?}"),
    }
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.remove(0), "key,type,ttl,value");
    lines.sort_unstable();
    assert_eq!(lines.len(), 6, "{text}");
    assert!(
        lines[0].starts_with("user:1,string,") && lines[0].ends_with(",\"\"\"hello world\"\"\""),
        "{}",
        lines[0]
    );
    let ttl: i64 = lines[0].split(',').nth(2).unwrap().parse().unwrap();
    assert!(ttl > 90_000_000, "{ttl}");
    assert_eq!(
        lines[1],
        "user:2,hash,,\"{\"\"name\"\":\"\"ann\"\",\"\"age\"\":\"\"30\"\"}\""
    );
    assert_eq!(lines[2], "user:3,list,,\"[\"\"a\"\",\"\"b\"\"]\"");
    assert_eq!(lines[3], "user:4,set,,\"[\"\"x\"\"]\"");
    assert_eq!(lines[4], "user:5,zset,,\"[[\"\"m\"\",1.5]]\"");
    assert_eq!(
        lines[5],
        "user:6,stream,,\"[{\"\"id\"\":\"\"1-1\"\",\"\"fields\"\":{\"\"f\"\":\"\"v\"\"}}]\""
    );
}

/// A query of a relational connection into a collection (no containers: SQLite and the fake
/// document driver).
#[test]
fn query_to_collection_as_flat_documents() {
    let dir = tempfile::tempdir().unwrap();
    let fake = sqlr_testkit::FakeDocDriver::new(Default::default());
    let mut drivers = DriverRegistry::default();
    drivers.register(sqlr_driver_sqlite::SqliteDriver::new());
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    let db = ConnectionConfig {
        values: [(
            "path".to_string(),
            dir.path().join("a.db").display().to_string(),
        )]
        .into(),
        ..config("s", "sqlite")
    };
    connect(&engine, db, Secrets::default(), Duration::from_secs(5));
    connect(
        &engine,
        config("d", "fakedoc"),
        Secrets::default(),
        Duration::from_secs(5),
    );
    sql_rows(&engine, "s", "CREATE TABLE t (id INTEGER, name TEXT)");
    sql_rows(&engine, "s", "INSERT INTO t VALUES (1, 'ann'), (2, NULL)");
    let spec = |sql: &str| TransferSpec {
        source: Endpoint::Query {
            conn: "s".into(),
            database: None,
            sql: sql.into(),
        },
        target: Endpoint::Collection {
            conn: "d".into(),
            target: people(),
            query: None,
        },
        mapping: Vec::new(),
        options: ImportOptions::default(),
    };
    match job(
        &engine,
        engine.transfer(spec("SELECT id, name FROM t ORDER BY id")),
    ) {
        JobOutcome::Imported {
            read: 2,
            written: 2,
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    let doc = |fields: Vec<(&str, DocValue)>| {
        DocValue::Document(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    };
    assert_eq!(
        fake.docs(),
        [
            doc(vec![
                ("id", DocValue::Int32(1)),
                ("name", DocValue::String("ann".into()))
            ]),
            doc(vec![("id", DocValue::Int32(2)), ("name", DocValue::Null)]),
        ]
    );
    // Only a statement that reads is a source.
    let JobOutcome::Failed(e) = job(&engine, engine.transfer(spec("DELETE FROM t"))) else {
        panic!("a write must be refused as a source")
    };
    assert!(e.message.contains("only reads"), "{}", e.message);
}

fn fake_doc_engine(docs: Vec<DocValue>) -> (Engine, std::sync::Arc<sqlr_testkit::FakeDocDriver>) {
    let fake = sqlr_testkit::FakeDocDriver::new(sqlr_testkit::FakeDocScript {
        docs,
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    connect(
        &engine,
        config("d", "fakedoc"),
        Secrets::default(),
        Duration::from_secs(5),
    );
    (engine, fake)
}

fn collection(query: Option<&str>, name: &str) -> Endpoint {
    Endpoint::Collection {
        conn: "d".into(),
        target: DocTarget {
            database: None,
            collection: name.into(),
        },
        query: query.map(String::from),
    }
}

#[test]
fn a_collection_source_only_reads() {
    let (engine, fake) = fake_doc_engine(vec![DocValue::Document(vec![(
        "_id".into(),
        DocValue::Int32(1),
    )])]);
    let dir = tempfile::tempdir().unwrap();
    let id = engine.transfer(TransferSpec {
        source: collection(Some("db.people.deleteMany({})"), "people"),
        target: Endpoint::File {
            path: dir.path().join("x.jsonl"),
            format: TextFormat::JsonLines,
        },
        mapping: Vec::new(),
        options: ImportOptions::default(),
    });
    let JobOutcome::Failed(e) = job(&engine, id) else {
        panic!("a write must be refused as a source")
    };
    assert!(e.message.contains("only reads"), "{}", e.message);
    assert!(fake.runs().iter().all(|(_, t)| !t.contains("deleteMany")));
}

#[test]
fn unmodelled_documents_are_never_transferred() {
    let code = DocValue::Document(vec![
        ("_id".into(), DocValue::Int32(1)),
        (
            "f".into(),
            DocValue::Document(vec![("$code".into(), DocValue::String("x".into()))]),
        ),
    ]);
    let plain = DocValue::Document(vec![("_id".into(), DocValue::Int32(2))]);
    let (engine, fake) = fake_doc_engine(vec![code, plain.clone()]);
    let spec = |skip: bool| TransferSpec {
        source: collection(None, "people"),
        target: collection(None, "copy"),
        mapping: Vec::new(),
        options: ImportOptions {
            skip_bad_rows: skip,
            ..Default::default()
        },
    };
    let JobOutcome::Failed(e) = job(&engine, engine.transfer(spec(false))) else {
        panic!("must fail")
    };
    assert!(e.message.contains("JavaScript code"), "{}", e.message);
    assert_eq!(fake.docs().len(), 2, "nothing was inserted");
    match job(&engine, engine.transfer(spec(true))) {
        JobOutcome::Imported {
            read: 2,
            written: 1,
            rejected,
        } => assert!(rejected[0].1.contains("JavaScript code")),
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.docs().last(), Some(&plain));
}

#[test]
fn a_binary_key_name_exports_exactly_that_key() {
    let page = |text: &[u8]| sqlr_core::KvPage::String {
        bytes: text.to_vec(),
        total_len: text.len() as u64,
    };
    let fake = sqlr_testkit::FakeKvDriver::new(sqlr_testkit::FakeKvScript {
        keys: vec![
            (b"bin:\xff".to_vec(), page(b"one")),
            (b"bin:\xfe".to_vec(), page(b"two")),
        ],
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake);
    let engine = Engine::new(drivers, no_repaint());
    connect(
        &engine,
        config("k", "fakekv"),
        Secrets::default(),
        Duration::from_secs(5),
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("key.csv");
    let id = engine.transfer(TransferSpec {
        source: Endpoint::RedisKeys {
            conn: "k".into(),
            db: 0,
            pattern: sqlr_core::glob_escape(b"bin:\xff"),
        },
        target: Endpoint::File {
            path: path.clone(),
            format: TextFormat::Csv(CsvOptions::default()),
        },
        mapping: Vec::new(),
        options: ImportOptions::default(),
    });
    match job(&engine, id) {
        JobOutcome::Exported { rows: 1, .. } => {}
        other => panic!("{other:?}"),
    }
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"\"\"one\"\"\""), "{text}");
}
