//! MongoDB driver against a `mongo:7` container.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlr_core::{
    Cell, Connection, ConnectionConfig, DocExecution, DocTarget, DocValue, DocumentConn, Driver,
    DriverError, ErrorKind, Node, NodeKind, NodePath, Secrets, StatementClass,
};
use sqlr_driver_mongo::MongoDriver;
use testcontainers_modules::mongo::Mongo;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

fn config(host: &str, port: u16, extra: &[(&str, &str)]) -> ConnectionConfig {
    let mut values: std::collections::BTreeMap<String, String> = [
        ("host", host.to_string()),
        ("port", port.to_string()),
        ("database", "test".to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for (k, v) in extra {
        values.insert(k.to_string(), v.to_string());
    }
    ConnectionConfig {
        id: "m".into(),
        name: "mongo".into(),
        driver: "mongodb".into(),
        group: None,
        color: None,
        values,
        profile: None,
        save_secrets: true,
    }
}

struct Server {
    _container: ContainerAsync<Mongo>,
    host: String,
    port: u16,
}

async fn start() -> Server {
    start_with(&[]).await
}

async fn start_with(env: &[(&str, &str)]) -> Server {
    let mut image = Mongo::default().with_tag("7");
    for (k, v) in env {
        image = image.with_env_var(*k, *v);
    }
    let c = image.start().await.expect("start mongo container");
    let host = c.get_host().await.expect("host").to_string();
    let port = c.get_host_port_ipv4(27017).await.expect("port");
    Server {
        _container: c,
        host,
        port,
    }
}

/// Connects, retrying while the server finishes starting (an image with a root user
/// restarts once after creating it).
async fn connect_to(
    server: &Server,
    extra: &[(&str, &str)],
    secrets: &Secrets,
) -> Result<Arc<dyn Connection>, DriverError> {
    let driver = MongoDriver::new();
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        match driver
            .connect(&config(&server.host, server.port, extra), secrets)
            .await
        {
            Err(e) if e.kind == ErrorKind::Connection && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            other => return other,
        }
    }
}

async fn connect(server: &Server) -> Arc<dyn Connection> {
    connect_to(server, &[], &Secrets::default())
        .await
        .expect("connect")
}

fn doc_conn(conn: &Arc<dyn Connection>) -> &dyn DocumentConn {
    conn.document().expect("a document connection")
}

async fn run(conn: &Arc<dyn Connection>, text: &str) -> DocExecution {
    match doc_conn(conn).run(None, text).await {
        Ok(x) => x,
        Err(e) => panic!("{text}: {e:?}"),
    }
}

async fn run_err(conn: &Arc<dyn Connection>, text: &str) -> DriverError {
    match doc_conn(conn).run(None, text).await {
        Ok(_) => panic!("{text}: expected an error"),
        Err(e) => e,
    }
}

/// All documents of a `Docs` result, fetched `page` at a time; also the page sizes seen.
async fn docs(exec: DocExecution, page: usize) -> (Vec<DocValue>, Vec<usize>) {
    let DocExecution::Docs(mut stream) = exec else {
        panic!("expected documents")
    };
    let (mut out, mut sizes) = (Vec::new(), Vec::new());
    while let Some(chunk) = stream.next_chunk(page).await.unwrap() {
        sizes.push(chunk.len());
        out.extend(chunk.docs());
    }
    stream.close().await.unwrap();
    (out, sizes)
}

async fn all_docs(conn: &Arc<dyn Connection>, text: &str) -> Vec<DocValue> {
    docs(run(conn, text).await, 500).await.0
}

fn value(exec: DocExecution) -> DocValue {
    match exec {
        DocExecution::Value(v) => v,
        _ => panic!("expected a value"),
    }
}

fn written(exec: DocExecution) -> (u64, u64, u64, u64, Vec<DocValue>) {
    match exec {
        DocExecution::Written {
            matched,
            modified,
            inserted,
            deleted,
            ids,
        } => (matched, modified, inserted, deleted, ids),
        _ => panic!("expected a write result"),
    }
}

/// Rows of a tabular result as text.
async fn rows(exec: DocExecution) -> (Vec<String>, Vec<Vec<String>>) {
    let DocExecution::Rows(mut stream) = exec else {
        panic!("expected rows")
    };
    let cols = stream.columns().iter().map(|c| c.name.clone()).collect();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next_chunk(100).await.unwrap() {
        for r in 0..chunk.len() {
            out.push(
                (0..chunk.columns().len())
                    .map(|c| match chunk.cell(r, c) {
                        Cell::Null => "NULL".to_string(),
                        other => other.to_string(),
                    })
                    .collect(),
            );
        }
    }
    stream.close().await.unwrap();
    (cols, out)
}

fn target(coll: &str) -> DocTarget {
    DocTarget {
        database: None,
        collection: coll.into(),
    }
}

fn names(nodes: &[Node]) -> Vec<(&str, NodeKind)> {
    nodes.iter().map(|n| (n.name.as_str(), n.kind)).collect()
}

/// Inserts `n` documents `{i, cat, name}` into `coll` in batches.
async fn seed(conn: &Arc<dyn Connection>, coll: &str, n: usize) {
    for start in (0..n).step_by(2000) {
        let docs: Vec<String> = (start..(start + 2000).min(n))
            .map(|i| {
                format!(
                    "{{i: {i}, cat: '{}', name: 'item {i}'}}",
                    ["a", "b", "c"][i % 3]
                )
            })
            .collect();
        let (_, _, inserted, _, _) =
            written(run(conn, &format!("db.{coll}.insertMany([{}])", docs.join(","))).await);
        assert_eq!(inserted as usize, docs.len());
    }
}

#[tokio::test]
async fn browse_tree() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let server = start().await;
    let conn = connect(&server).await;
    let info = conn.ping().await.unwrap();
    assert_eq!(info.product, "MongoDB");
    assert!(info.version.starts_with("7."), "{}", info.version);

    let d = doc_conn(&conn);
    d.run(Some("shop"), "db.orders.insertOne({total: 5})")
        .await
        .unwrap();
    d.run(
        Some("shop"),
        "db.orders.createIndex({total: -1}, {unique: true})",
    )
    .await
    .unwrap();
    d.run(
        Some("shop"),
        "db.runCommand({create: 'big_orders', viewOn: 'orders', pipeline: [{$match: {total: {$gt: 1}}}]})",
    )
    .await
    .unwrap();

    let root = conn.browse(&NodePath::root()).await.unwrap();
    let shop = root.iter().find(|n| n.name == "shop").expect("shop listed");
    assert_eq!(shop.kind, NodeKind::Database);
    assert!(root.iter().any(|n| n.name == "admin"));

    let folders = conn.browse(&shop.path).await.unwrap();
    assert_eq!(
        names(&folders),
        [
            ("Collections", NodeKind::Folder),
            ("Views", NodeKind::Folder),
            ("System", NodeKind::Folder)
        ]
    );
    let colls = conn.browse(&folders[0].path).await.unwrap();
    assert_eq!(names(&colls), [("orders", NodeKind::Table)]);
    assert!(colls[0].has_children);
    let views = conn.browse(&folders[1].path).await.unwrap();
    assert_eq!(names(&views), [("big_orders", NodeKind::View)]);
    let in_shop = |coll: &str| DocTarget {
        database: Some("shop".into()),
        collection: coll.into(),
    };
    assert!(d.is_view(&in_shop("big_orders")).await.unwrap());
    assert!(!d.is_view(&in_shop("orders")).await.unwrap());
    assert!(!d.is_view(&in_shop("missing")).await.unwrap());
    let system = conn.browse(&folders[2].path).await.unwrap();
    assert_eq!(names(&system), [("system.views", NodeKind::Table)]);

    let indexes = conn.browse(&colls[0].path).await.unwrap();
    assert_eq!(
        names(&indexes),
        [("_id_", NodeKind::Index), ("total_-1", NodeKind::Index)]
    );
    assert_eq!(indexes[1].detail.as_deref(), Some("{\"total\": -1} unique"));
    assert_eq!(conn.database_name(), Some("test"));
}

#[tokio::test]
async fn find_pages_with_sort_limit_skip_projection() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let server = start().await;
    let conn = connect(&server).await;
    seed(&conn, "big", 10_000).await;

    // Every document, page by page, never more than a page in a chunk.
    let (all, sizes) = docs(run(&conn, "db.big.find()").await, 500).await;
    assert_eq!(all.len(), 10_000);
    assert!(sizes.iter().all(|&s| s <= 500), "{sizes:?}");
    assert!(sizes.len() >= 20);

    let (page, _) = docs(
        run(
            &conn,
            "db.big.find({cat: {$in: ['a', 'b']}}, {name: 1, _id: 0})\n  .sort({i: -1})\n  .skip(10)\n  .limit(3000)\n  .projection({i: 1, _id: 0})",
        )
        .await,
        500,
    )
    .await;
    assert_eq!(page.len(), 3000);
    assert_eq!(
        page[0],
        DocValue::Document(vec![("i".into(), DocValue::Int32(9984))])
    );
    let is: Vec<i32> = page
        .iter()
        .map(|d| match d.get("i") {
            Some(DocValue::Int32(i)) => *i,
            other => panic!("{other:?}"),
        })
        .collect();
    assert!(is.windows(2).all(|w| w[0] > w[1]), "sorted descending");
    assert!(is.iter().all(|i| i % 3 != 2), "filtered");

    let (one, _) = docs(run(&conn, "db.big.findOne({i: 7})").await, 500).await;
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].get("name"), Some(&DocValue::String("item 7".into())));

    // Closing a stream early is fine.
    let DocExecution::Docs(mut s) = run(&conn, "db.big.find()").await else {
        panic!()
    };
    assert_eq!(s.next_chunk(10).await.unwrap().unwrap().len(), 10);
    s.close().await.unwrap();
    s.close().await.unwrap();
}

#[tokio::test]
async fn aggregate_count_and_distinct() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let server = start().await;
    let conn = connect(&server).await;
    seed(&conn, "items", 300).await;

    let groups = all_docs(
        &conn,
        "db.items.aggregate([{ $group: { _id: '$cat', n: { $sum: 1 } } }, { $sort: { _id: 1 } }])",
    )
    .await;
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].get("_id"), Some(&DocValue::String("a".into())));
    assert_eq!(groups[0].get("n"), Some(&DocValue::Int32(100)));

    assert_eq!(
        value(run(&conn, "db.items.countDocuments({cat: 'b'})").await),
        DocValue::Int64(100)
    );
    assert_eq!(
        value(run(&conn, "db.items.countDocuments({}, {skip: 10, limit: 5})").await),
        DocValue::Int64(5)
    );
    assert_eq!(
        value(run(&conn, "db.items.countDocuments({cat: 'zzz'})").await),
        DocValue::Int64(0)
    );
    assert_eq!(
        value(run(&conn, "db.items.estimatedDocumentCount()").await),
        DocValue::Int64(300)
    );

    let (cols, values) = rows(run(&conn, "db.items.distinct('cat', {i: {$lt: 2}})").await).await;
    assert_eq!(cols, ["cat"]);
    assert_eq!(values, [["a"], ["b"]]);
}

#[tokio::test]
async fn writes_report_counts() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let server = start().await;
    let conn = connect(&server).await;

    let (_, _, inserted, _, ids) = written(run(&conn, "db.w.insertOne({a: 1})").await);
    assert_eq!(inserted, 1);
    assert!(matches!(ids[..], [DocValue::ObjectId(_)]));
    let (_, _, inserted, _, ids) =
        written(run(&conn, "db.w.insertMany([{_id: 10, a: 2}, {_id: 11, a: 2}])").await);
    assert_eq!(
        (inserted, ids),
        (2, vec![DocValue::Int32(10), DocValue::Int32(11)])
    );

    let w = written(run(&conn, "db.w.updateOne({a: 1}, {$set: {b: 1}})").await);
    assert_eq!((w.0, w.1, w.2, w.3), (1, 1, 0, 0));
    let w = written(
        run(
            &conn,
            "db.w.updateMany({a: 2}, [{$set: {b: {$add: ['$a', 1]}}}])",
        )
        .await,
    );
    assert_eq!((w.0, w.1, w.2, w.3), (2, 2, 0, 0));
    let w = written(
        run(
            &conn,
            "db.w.updateOne({a: 9}, {$set: {b: 9}}, {upsert: true})",
        )
        .await,
    );
    assert_eq!((w.0, w.1, w.2, w.3), (0, 0, 1, 0));
    assert!(matches!(w.4[..], [DocValue::ObjectId(_)]));
    let w = written(run(&conn, "db.w.replaceOne({_id: 10}, {a: 3})").await);
    assert_eq!((w.0, w.1), (1, 1));
    let w = written(run(&conn, "db.w.deleteOne({_id: 11})").await);
    assert_eq!(w.3, 1);

    let after = value(
        run(
            &conn,
            "db.w.findOneAndUpdate({_id: 10}, {$inc: {n: 5}}, {returnDocument: 'after', projection: {n: 1}})",
        )
        .await,
    );
    assert_eq!(
        after,
        DocValue::Document(vec![
            ("_id".into(), DocValue::Int32(10)),
            ("n".into(), DocValue::Int32(5))
        ])
    );

    assert_eq!(
        value(run(&conn, "db.w.createIndex({a: 1, b: -1})").await),
        DocValue::String("a_1_b_-1".into())
    );
    let indexes = all_docs(&conn, "db.w.getIndexes()").await;
    assert_eq!(indexes.len(), 2);
    assert_eq!(
        indexes[1].get("name"),
        Some(&DocValue::String("a_1_b_-1".into()))
    );
    run(&conn, "db.w.dropIndex('a_1_b_-1')").await;
    assert_eq!(
        doc_conn(&conn).indexes(&target("w")).await.unwrap().len(),
        1
    );

    // Duplicate keys are a clear error.
    let e = run_err(&conn, "db.w.insertOne({_id: 10})").await;
    assert!(e.message.contains("duplicate key"), "{}", e.message);

    // Document edits by `_id`.
    let d = doc_conn(&conn);
    let id = d
        .insert_doc(
            &target("w"),
            &DocValue::Document(vec![("x".into(), DocValue::Int32(1))]),
        )
        .await
        .unwrap();
    assert!(matches!(id, DocValue::ObjectId(_)));
    let replacement = DocValue::Document(vec![
        ("_id".into(), id.clone()),
        ("x".into(), DocValue::Int32(2)),
    ]);
    assert_eq!(
        d.replace_doc(&target("w"), &id, &replacement)
            .await
            .unwrap(),
        1
    );
    // A replacement with update operators would update instead of replace: refused.
    let operators = DocValue::Document(vec![(
        "$set".into(),
        DocValue::Document(vec![("x".into(), DocValue::Int32(9))]),
    )]);
    let e = d
        .replace_doc(&target("w"), &id, &operators)
        .await
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Syntax, "{e:?}");
    assert_eq!(
        d.replace_doc(&target("w"), &DocValue::Int32(404), &replacement)
            .await
            .unwrap(),
        0
    );
    let found = all_docs(&conn, "db.w.find({x: 2})").await;
    assert_eq!(found, [replacement]);
    assert_eq!(d.delete_doc(&target("w"), &id).await.unwrap(), 1);
    assert_eq!(d.delete_doc(&target("w"), &id).await.unwrap(), 0);

    let w = written(run(&conn, "db.w.deleteMany({})").await);
    assert_eq!(w.3, 3);
    assert!(matches!(
        d.classify("db.w.deleteMany({})"),
        StatementClass::Destructive(_)
    ));
    run(&conn, "db.w.drop()").await;
    assert!(all_docs(&conn, "db.w.find()").await.is_empty());
}

#[tokio::test]
async fn run_command_and_use() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let server = start().await;
    let conn = connect(&server).await;

    let ping = value(run(&conn, "db.runCommand({ping: 1})").await);
    assert_eq!(ping.get("ok"), Some(&DocValue::Double(1.0)));
    let e = run_err(&conn, "db.runCommand({noSuchCommand: 1})").await;
    assert!(e.message.contains("no such command"), "{}", e.message);

    assert_eq!(
        value(run(&conn, "use other").await),
        DocValue::String("switched to db other".into())
    );
    run(&conn, "db.c.insertOne({a: 1})").await;
    let d = doc_conn(&conn);
    let DocExecution::Docs(mut s) = d.run(Some("other"), "db.c.find()").await.unwrap() else {
        panic!()
    };
    assert_eq!(s.next_chunk(10).await.unwrap().unwrap().len(), 1);
    // `database` overrides the current one for one call.
    let DocExecution::Docs(mut s) = d.run(Some("test"), "db.c.find()").await.unwrap() else {
        panic!()
    };
    assert!(s.next_chunk(10).await.unwrap().is_none());

    let (cols, dbs) = rows(run(&conn, "show dbs").await).await;
    assert_eq!(cols[0], "name");
    assert!(dbs.iter().any(|r| r[0] == "other"), "{dbs:?}");
    let (_, colls) = rows(run(&conn, "show collections").await).await;
    assert_eq!(colls, [["c", "collection"]]);
    assert_eq!(
        value(run(&conn, "db.getCollectionNames()").await),
        DocValue::Array(vec![DocValue::String("c".into())])
    );

    // One statement per call; parse errors carry their position.
    let e = run_err(&conn, "db.c.find()\ndb.c.find()").await;
    assert_eq!(e.kind, ErrorKind::Syntax);
    let e = run_err(&conn, "db.c.find().forEach(printjson)").await;
    assert_eq!((e.kind, e.position), (ErrorKind::Syntax, Some(12)));
    assert_eq!(
        d.open_target_text(&target("c"), 500),
        "db.c.find({}).limit(500)"
    );
    assert_eq!(
        d.open_target_text(&target("my-coll"), 50),
        "db.getCollection(\"my-coll\").find({}).limit(50)"
    );
}

/// One value of every BSON type DocValue models.
fn matrix() -> DocValue {
    let d = |pairs: Vec<(&str, DocValue)>| {
        DocValue::Document(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    };
    d(vec![
        (
            "_id",
            DocValue::ObjectId(*b"\x65\x0a\xbc\xde\xf0\x12\x34\x56\x78\x9a\xbc\xde"),
        ),
        ("null", DocValue::Null),
        ("bool", DocValue::Bool(false)),
        ("i32", DocValue::Int32(i32::MIN)),
        ("i64_small", DocValue::Int64(5)),
        ("i64", DocValue::Int64(i64::MAX)),
        ("dbl", DocValue::Double(2.0)),
        ("dbl_frac", DocValue::Double(-0.125)),
        ("nan", DocValue::Double(f64::NAN)),
        ("inf", DocValue::Double(f64::NEG_INFINITY)),
        (
            "dec",
            DocValue::Decimal128("1234567890.123456789012345678901234".into()),
        ),
        ("dec_scale", DocValue::Decimal128("1.10".into())),
        ("str", DocValue::String("è😀 \"q\" \\ \n".into())),
        ("date", DocValue::DateTime(1_700_000_000_123)),
        ("date_old", DocValue::DateTime(-62_135_596_800_000)),
        ("date_far", DocValue::DateTime(253_402_300_800_005)),
        (
            "bin",
            DocValue::Binary {
                subtype: 0,
                bytes: vec![0, 1, 255],
            },
        ),
        (
            "uuid",
            DocValue::Binary {
                subtype: 4,
                bytes: (0u8..16).collect(),
            },
        ),
        (
            "user_bin",
            DocValue::Binary {
                subtype: 0x80,
                bytes: vec![7],
            },
        ),
        (
            "re",
            DocValue::Regex {
                pattern: "^a[/]b".into(),
                options: "im".into(),
            },
        ),
        (
            "ts",
            DocValue::Timestamp {
                t: 1_700_000_000,
                i: 3,
            },
        ),
        ("min", DocValue::MinKey),
        ("max", DocValue::MaxKey),
        ("undef", DocValue::Undefined),
        ("a.b", DocValue::String("dotted".into())),
        (
            "arr",
            DocValue::Array(vec![DocValue::Int32(1), DocValue::Array(vec![]), d(vec![])]),
        ),
        ("nested", d(vec![("x", d(vec![("y", DocValue::Null)]))])),
    ])
}

#[tokio::test]
async fn typed_values_matrix() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let server = start().await;
    let conn = connect(&server).await;
    let original = matrix();
    // Through the shell text the JSON view would show…
    let text = format!("db.types.insertOne({})", original.to_relaxed_json());
    run(&conn, &text).await;
    // …and through the document API.
    let mut second = original.clone();
    if let DocValue::Document(f) = &mut second {
        f[0].1 = DocValue::Int32(2);
    }
    doc_conn(&conn)
        .insert_doc(&target("types"), &second)
        .await
        .unwrap();

    let back = all_docs(&conn, "db.types.find().sort({_id: -1})").await;
    assert_eq!(back.len(), 2);
    for (got, want) in back.iter().zip([&original, &second]) {
        // NaN != NaN: compare canonical Extended JSON.
        assert_eq!(got.to_canonical_ejson(), want.to_canonical_ejson());
    }
    // Typed literals query typed values.
    let hits = all_docs(&conn, "db.types.find({i64_small: NumberLong(5), dec_scale: NumberDecimal('1.10'), date: ISODate('2023-11-14T22:13:20.123Z')}, {_id: 1})").await;
    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn cancel_a_long_aggregate_kills_it() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let server = start().await;
    let conn = connect(&server).await;
    seed(&conn, "slow", 300).await;
    let cancel = conn.cancel_handle();
    let c2 = conn.clone();
    let task = tokio::spawn(async move {
        let started = Instant::now();
        let r = doc_conn(&c2)
            .run(
                None,
                "db.slow.aggregate([{$match: {$expr: {$function: {body: 'function() { sleep(100); return true; }', args: [], lang: 'js'}}}}])",
            )
            .await;
        (r.map(|_| ()), started.elapsed())
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    cancel.cancel().await.expect("cancel");
    let (r, took) = task.await.unwrap();
    let e = r.expect_err("the aggregate was killed");
    assert_eq!(e.kind, ErrorKind::Cancelled, "{e:?}");
    assert!(took < Duration::from_secs(8), "{took:?}");
    // The connection keeps working.
    assert_eq!(
        value(run(&conn, "db.slow.estimatedDocumentCount()").await),
        DocValue::Int64(300)
    );
}

#[tokio::test]
async fn auth_failure_message() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let server = start_with(&[
        ("MONGO_INITDB_ROOT_USERNAME", "root"),
        ("MONGO_INITDB_ROOT_PASSWORD", "s3cret"),
    ])
    .await;
    let user = [("user", "root")];
    let good = Secrets([("password".to_string(), "s3cret".to_string())].into());
    // Wait for the restarted server.
    connect_to(&server, &user, &good)
        .await
        .expect("right password");

    let bad = Secrets([("password".to_string(), "wrong".to_string())].into());
    let e = match connect_to(&server, &user, &bad).await {
        Ok(_) => panic!("a wrong password must fail"),
        Err(e) => e,
    };
    assert_eq!(e.kind, ErrorKind::Auth, "{e:?}");
    assert!(e.message.contains("Authentication failed"), "{}", e.message);
    assert!(!e.message.contains("wrong"), "no secret in the message");

    let e = match connect_to(&server, &[], &Secrets::default()).await {
        Ok(c) => match c.browse(&NodePath::root()).await {
            Ok(_) => panic!("no credentials must not list databases"),
            Err(e) => e,
        },
        Err(e) => e,
    };
    assert!(
        matches!(e.kind, ErrorKind::Permission | ErrorKind::Auth),
        "{e:?}"
    );
}

#[tokio::test]
async fn read_only_refuses_writes() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let server = start().await;
    let rw = connect(&server).await;
    run(&rw, "db.c.insertOne({_id: 1})").await;

    let ro = connect_to(&server, &[("read_only", "true")], &Secrets::default())
        .await
        .unwrap();
    let d = doc_conn(&ro);
    assert_eq!(all_docs(&ro, "db.c.find()").await.len(), 1);
    for text in [
        "db.c.insertOne({a: 1})",
        "db.c.deleteMany({})",
        "db.c.updateOne({_id: 1}, {$set: {a: 1}})",
        "db.c.drop()",
        "db.c.createIndex({a: 1})",
        "db.c.aggregate([{$out: 'copy'}])",
        "db.runCommand({insert: 'c', documents: [{a: 1}]})",
        "db.runCommand({shutdown: 1})",
    ] {
        let e = run_err(&ro, text).await;
        assert_eq!(e.kind, ErrorKind::Permission, "{text}");
        assert_eq!(e.message, "The session is read-only");
    }
    assert_eq!(
        d.delete_doc(&target("c"), &DocValue::Int32(1))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Permission
    );
    assert!(
        d.insert_doc(&target("c"), &DocValue::Document(vec![]))
            .await
            .is_err()
    );
    assert_eq!(
        all_docs(&ro, "db.c.find()").await.len(),
        1,
        "nothing written"
    );
    // Reads still run, including read-only commands.
    value(run(&ro, "db.runCommand({listCollections: 1})").await);
    run(&ro, "db.c.countDocuments()").await;

    d.set_read_only(false);
    run(&ro, "db.c.insertOne({_id: 2})").await;
    d.set_read_only(true);
    assert_eq!(
        run_err(&ro, "db.c.insertOne({_id: 3})").await.kind,
        ErrorKind::Permission
    );
}

#[tokio::test]
async fn chunks_stop_at_a_byte_cap() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let server = start().await;
    let conn = connect(&server).await;
    let big = "x".repeat(1_200_000);
    for i in 0..40 {
        let doc = DocValue::Document(vec![
            ("_id".into(), DocValue::Int32(i)),
            ("blob".into(), DocValue::String(big.clone())),
        ]);
        doc_conn(&conn)
            .insert_doc(&target("big"), &doc)
            .await
            .unwrap();
    }
    let (all, sizes) = docs(run(&conn, "db.big.find().sort({_id: 1})").await, 500).await;
    assert_eq!(all.len(), 40, "nothing lost at a chunk boundary");
    assert!(sizes[0] < 40, "a chunk stops near 32 MB: {sizes:?}");
    assert!(sizes.len() >= 2);
    let ids: Vec<&DocValue> = all.iter().map(|d| d.get("_id").unwrap()).collect();
    assert_eq!(
        ids,
        (0..40)
            .map(DocValue::Int32)
            .collect::<Vec<_>>()
            .iter()
            .collect::<Vec<_>>()
    );
}

/// A filter that sleeps 100 ms per document on the server.
const SLOW: &str =
    "{$expr: {$function: {body: 'function() { sleep(100); return true; }', args: [], lang: 'js'}}}";

/// Runs `text` in a task and cancels it after 1.5 s (first running a browse on the same
/// connection when `meanwhile`); returns the error and how long the statement took.
async fn cancel_after(
    conn: &Arc<dyn Connection>,
    text: String,
    meanwhile: bool,
) -> (DriverError, Duration) {
    let cancel = conn.cancel_handle();
    let c2 = conn.clone();
    let task = tokio::spawn(async move {
        let started = Instant::now();
        let r = doc_conn(&c2).run(None, &text).await;
        (r.map(|_| ()), started.elapsed())
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    if meanwhile {
        // The browse finishes before the cancel; it must not take the running
        // statement's tag with it.
        conn.browse(&NodePath::root()).await.unwrap();
    }
    cancel.cancel().await.expect("cancel");
    let (r, took) = task.await.unwrap();
    (r.expect_err("the statement was killed"), took)
}

#[tokio::test]
async fn cancel_kills_counts_distinct_and_survives_a_browse() {
    if std::env::var("SQLR_SKIP_CONTAINER_TESTS").is_ok_and(|v| v == "1") {
        return;
    }
    let server = start().await;
    let conn = connect(&server).await;
    seed(&conn, "slow", 300).await;
    for (text, meanwhile) in [
        (format!("db.slow.countDocuments({SLOW})"), false),
        (format!("db.slow.distinct('cat', {SLOW})"), false),
        (format!("db.slow.deleteMany({SLOW})"), false),
        (format!("db.slow.find({SLOW})"), true),
    ] {
        let (e, took) = cancel_after(&conn, text.clone(), meanwhile).await;
        assert_eq!(e.kind, ErrorKind::Cancelled, "{text}: {e:?}");
        assert!(took < Duration::from_secs(8), "{text}: {took:?}");
    }
}
