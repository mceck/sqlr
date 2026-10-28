//! Document results and edits: paging, fetch all and memory budgets,
//! edits by `_id` (and `_index`), read-only refusals. The first tests use the fake document
//! driver; the last ones run against the fixed MongoDB and Elasticsearch services.

use crate::support;

use std::sync::Arc;
use std::time::Duration;

use sqlr_core::{
    ConnectionConfig, ConnectionProfile, DocTarget, DocValue, ErrorKind, Secrets, new_object_id,
};
use sqlr_engine::{
    DocResult, DriverRegistry, Engine, JobId, JobOutcome, QueryId, QueryStatus, SessionKey,
    SessionStatus,
};
use sqlr_testkit::{FakeDocDriver, FakeDocScript};
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn doc(fields: Vec<(&str, DocValue)>) -> DocValue {
    DocValue::Document(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
}

fn numbered(n: usize) -> Vec<DocValue> {
    (0..n)
        .map(|i| {
            doc(vec![
                ("_id", DocValue::Int32(i as i32)),
                ("n", DocValue::Int32(i as i32)),
            ])
        })
        .collect()
}

fn fake_engine(script: FakeDocScript, profile: ConnectionProfile) -> (Engine, Arc<FakeDocDriver>) {
    let fake = FakeDocDriver::new(script);
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("d", "fakedoc"), Secrets::default(), profile);
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("d"),
        Some(SessionStatus::Connected(_))
    )));
    (engine, fake)
}

fn meta() -> SessionKey {
    SessionKey::meta("d")
}

fn users() -> DocTarget {
    DocTarget {
        database: None,
        collection: "users".into(),
    }
}

fn settled(engine: &Engine, q: QueryId) -> QueryStatus {
    assert!(
        wait_until(TIMEOUT * 6, || engine
            .doc_result(q)
            .is_some_and(|r| r.read().status != QueryStatus::Running)),
        "the run never settled"
    );
    engine.doc_result(q).unwrap().read().status.clone()
}

fn count(engine: &Engine, q: QueryId) -> usize {
    engine.doc_result(q).map_or(0, |r| r.read().len())
}

fn job(engine: &Engine, id: JobId) -> JobOutcome {
    assert!(
        wait_until(TIMEOUT * 6, || engine
            .job(id)
            .is_some_and(|j| j.outcome.is_some())),
        "the job never finished"
    );
    engine.job(id).unwrap().outcome.unwrap()
}

fn docs_of(r: &DocResult) -> Vec<DocValue> {
    (0..r.len()).filter_map(|i| r.doc(i)).collect()
}

// ----- fake driver -----

#[test]
fn doc_results_page_fetch_more_and_fetch_all() {
    let (engine, _) = fake_engine(
        FakeDocScript {
            docs: numbered(1_200),
            ..Default::default()
        },
        dev(),
    );
    let q = engine.run_doc(&meta(), None, "db.users.find({})".into(), 500);
    assert_eq!(settled(&engine, q), QueryStatus::Ready { more: true });
    assert_eq!(count(&engine, q), 500);
    {
        let r = engine.doc_result(q).unwrap();
        let r = r.read();
        assert_eq!(
            r.target,
            Some(users()),
            "find() results know their collection"
        );
        assert_eq!(r.read_only, None, "and can be edited");
        assert!(!r.tabular);
        assert_eq!(r.doc(499).unwrap().get("n"), Some(&DocValue::Int32(499)));
    }
    engine.fetch_more_docs(q);
    assert!(wait_until(TIMEOUT, || count(&engine, q) == 1_000));
    assert_eq!(settled(&engine, q), QueryStatus::Ready { more: true });
    engine.fetch_all_docs(q);
    assert!(wait_until(TIMEOUT, || count(&engine, q) == 1_200));
    assert_eq!(settled(&engine, q), QueryStatus::Ready { more: false });
    engine.close_docs(q);
    assert!(engine.doc_result(q).is_none());
}

#[test]
fn a_script_keeps_each_statements_first_page() {
    let (engine, _) = fake_engine(
        FakeDocScript {
            docs: numbered(30),
            ..Default::default()
        },
        dev(),
    );
    let qs: Vec<QueryId> = (0..3)
        .map(|_| engine.run_doc(&meta(), None, "db.users.find({})".into(), 10))
        .collect();
    for q in &qs {
        settled(&engine, *q);
        assert_eq!(
            count(&engine, *q),
            10,
            "each statement shows its first page"
        );
    }
    // The earlier ones were cut short by the later ones; the last can go on.
    let r = engine.doc_result(qs[0]).unwrap();
    assert_eq!(r.read().status, QueryStatus::Ready { more: false });
    assert!(r.read().cut_short);
    assert_eq!(
        engine.doc_result(qs[2]).unwrap().read().status,
        QueryStatus::Ready { more: true }
    );
}

#[test]
fn fetch_all_stops_at_the_result_memory_budget() {
    let big = |i: usize| {
        doc(vec![
            ("_id", DocValue::Int32(i as i32)),
            ("blob", DocValue::String("x".repeat(10_000))),
        ])
    };
    let (engine, _) = fake_engine(
        FakeDocScript {
            docs: (0..400).map(big).collect(),
            ..Default::default()
        },
        dev(),
    );
    engine.set_doc_budget(1_000_000);
    let q = engine.run_doc(&meta(), None, "db.users.find({})".into(), 50);
    settled(&engine, q);
    engine.fetch_all_docs(q);
    // At the budget the result ends: no more Fetch more, one warning.
    assert!(wait_until(TIMEOUT, || engine.doc_result(q).is_some_and(
        |r| r.read().status == QueryStatus::Ready { more: false }
            && r.read().messages.iter().any(|m| m.text.contains("memory"))
    )));
    engine.fetch_all_docs(q);
    std::thread::sleep(Duration::from_millis(50));
    let warnings = engine
        .doc_result(q)
        .unwrap()
        .read()
        .messages
        .iter()
        .filter(|m| m.text.contains("memory"))
        .count();
    assert_eq!(warnings, 1);
    let r = engine.doc_result(q).unwrap();
    let r = r.read();
    assert!(r.len() < 400, "{} documents kept", r.len());
    assert!(
        r.byte_size() <= 1_000_000 + 50 * 11_000,
        "{}",
        r.byte_size()
    );
}

#[test]
fn only_plain_find_results_are_editable() {
    let (engine, _) = fake_engine(
        FakeDocScript {
            docs: numbered(3),
            ..Default::default()
        },
        dev(),
    );
    for text in [
        "db.users.find({}, {n: 1})",
        "db.users.aggregate([{$match: {}}])",
    ] {
        let q = engine.run_doc(&meta(), None, text.into(), 10);
        settled(&engine, q);
        let r = engine.doc_result(q).unwrap();
        let r = r.read();
        assert!(r.read_only.is_some(), "{text}");
        assert_eq!(r.len(), 3);
    }
}

#[test]
fn a_find_on_a_view_is_read_only() {
    let (engine, _) = fake_engine(
        FakeDocScript {
            docs: numbered(3),
            views: vec!["active_users".into()],
            ..Default::default()
        },
        dev(),
    );
    let q = engine.run_doc(&meta(), None, "db.active_users.find({})".into(), 10);
    settled(&engine, q);
    let r = engine.doc_result(q).unwrap();
    let reason = r.read().read_only.clone();
    assert!(
        reason.as_deref().is_some_and(|m| m.contains("view")),
        "{reason:?}"
    );
    // A collection's plain find stays editable.
    let q = engine.run_doc(&meta(), None, "db.users.find({})".into(), 10);
    settled(&engine, q);
    assert_eq!(engine.doc_result(q).unwrap().read().read_only, None);
}

#[test]
fn values_and_write_counts_show_as_documents() {
    let value = doc(vec![("ok", DocValue::Double(1.0))]);
    let (engine, _) = fake_engine(
        FakeDocScript {
            value: Some(value.clone()),
            ..Default::default()
        },
        dev(),
    );
    let q = engine.run_doc(&meta(), None, "db.runCommand({ping: 1})".into(), 10);
    assert_eq!(settled(&engine, q), QueryStatus::Ready { more: false });
    let r = engine.doc_result(q).unwrap();
    let r = r.read();
    assert_eq!(docs_of(&r), [value]);
    assert!(r.read_only.is_some());
    assert!(!r.messages.is_empty(), "the outcome is in the messages");
}

#[test]
fn save_insert_and_delete_documents_by_id() {
    let (engine, fake) = fake_engine(
        FakeDocScript {
            docs: numbered(3),
            ..Default::default()
        },
        dev(),
    );
    let changed = doc(vec![
        ("_id", DocValue::Int32(1)),
        ("n", DocValue::String("one".into())),
    ]);
    let saved = engine.save_doc(&meta(), users(), Some(DocValue::Int32(1)), changed.clone());
    assert!(
        matches!(
            job(&engine, saved),
            JobOutcome::DocsWritten {
                requested: 1,
                written: 1,
                ..
            }
        ),
        "replaced by _id"
    );
    assert_eq!(fake.docs()[1], changed);
    // A document deleted meanwhile: nothing is replaced, and the save says so.
    let gone = engine.save_doc(&meta(), users(), Some(DocValue::Int32(99)), changed);
    let JobOutcome::Failed(e) = job(&engine, gone) else {
        panic!("saving a missing document must fail")
    };
    assert!(e.message.contains("no longer exists"), "{}", e.message);
    let new = doc(vec![("_id", DocValue::Int32(7))]);
    let inserted = engine.save_doc(&meta(), users(), None, new);
    assert!(matches!(
        job(&engine, inserted),
        JobOutcome::DocsWritten {
            written: 1,
            id: Some(DocValue::Int32(7)),
            ..
        }
    ));
    let deleted = engine.delete_docs(
        &meta(),
        users(),
        vec![DocValue::Int32(0), DocValue::Int32(42), DocValue::Int32(7)],
    );
    assert_eq!(
        job(&engine, deleted),
        JobOutcome::DocsWritten {
            requested: 3,
            written: 2,
            id: None
        }
    );
    assert_eq!(fake.docs().len(), 2);
}

#[test]
fn unmodelled_types_are_never_saved() {
    let code = doc(vec![
        ("_id", DocValue::Int32(1)),
        (
            "f",
            doc(vec![("$code", DocValue::String("function () {}".into()))]),
        ),
    ]);
    let (engine, fake) = fake_engine(
        FakeDocScript {
            docs: vec![code.clone()],
            ..Default::default()
        },
        dev(),
    );
    let id = engine.save_doc(&meta(), users(), Some(DocValue::Int32(1)), code.clone());
    let JobOutcome::Failed(e) = job(&engine, id) else {
        panic!("must be refused")
    };
    assert!(e.message.contains("JavaScript code"), "{}", e.message);
    assert_eq!(fake.docs(), [code], "nothing was written");
}

#[test]
fn read_only_sessions_refuse_document_writes() {
    let read_only = ConnectionProfile {
        read_only: true,
        ..dev()
    };
    let (engine, fake) = fake_engine(
        FakeDocScript {
            docs: numbered(2),
            ..Default::default()
        },
        read_only,
    );
    let q = engine.run_doc(&meta(), None, "db.users.find({})".into(), 10);
    settled(&engine, q);
    assert!(
        engine.doc_result(q).unwrap().read().read_only.is_some(),
        "a read-only session's documents are not editable"
    );
    let write = engine.run_doc(&meta(), None, "db.users.deleteMany({})".into(), 10);
    let QueryStatus::Failed(e) = settled(&engine, write) else {
        panic!("a write must be refused")
    };
    assert_eq!(e.kind, ErrorKind::Permission);
    for id in [
        engine.save_doc(
            &meta(),
            users(),
            Some(DocValue::Int32(0)),
            numbered(1)[0].clone(),
        ),
        engine.delete_docs(&meta(), users(), vec![DocValue::Int32(0)]),
        engine.doc_admin(&meta(), None, "db.users.createIndex({n: 1})".into()),
    ] {
        let JobOutcome::Failed(e) = job(&engine, id) else {
            panic!("must be refused")
        };
        assert_eq!(e.kind, ErrorKind::Permission);
    }
    assert_eq!(fake.docs().len(), 2);
    assert!(
        !fake.runs().iter().any(|(_, t)| t.contains("deleteMany")),
        "the engine refused it before the driver saw it"
    );
}

#[test]
fn index_admin_runs_the_statement_as_a_job() {
    let (engine, fake) = fake_engine(FakeDocScript::default(), dev());
    let id = engine.doc_admin(
        &meta(),
        Some("app".into()),
        "db.getCollection(\"users\").createIndex({\"n\": 1})".into(),
    );
    assert_eq!(job(&engine, id), JobOutcome::Done);
    assert_eq!(
        fake.runs(),
        [(
            Some("app".to_string()),
            "db.getCollection(\"users\").createIndex({\"n\": 1})".to_string()
        )]
    );
    assert_eq!(
        engine.doc_classify("d", "db.users.drop()"),
        sqlr_core::StatementClass::Destructive("drop() drops the collection users".into())
    );
}

// ----- MongoDB -----

mod mongo {
    use super::*;
    use sqlr_testkit::TestLock;

    pub struct Server {
        pub engine: Engine,
        _lock: TestLock,
        _rt: tokio::runtime::Runtime,
    }

    pub fn start() -> Server {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (host, port) = support::endpoint("MONGO");
        let lock = TestLock::acquire("mongo");
        rt.block_on(support::reset_mongo(&host, port));
        let mut drivers = DriverRegistry::default();
        drivers.register(support::mongo_sidecar());
        let engine = Engine::new(drivers, no_repaint());
        engine.set_connect_timeout(60);
        let cfg = ConnectionConfig {
            values: [
                ("host".to_string(), host),
                ("port".to_string(), port.to_string()),
                ("database".to_string(), "test".to_string()),
            ]
            .into(),
            ..config("m", "mongodb")
        };
        // The server may still be starting: retry the connect.
        for _ in 0..60 {
            engine.connect(cfg.clone(), Secrets::default(), dev());
            wait_until(Duration::from_secs(70), || {
                !matches!(engine.session_status("m"), Some(SessionStatus::Connecting))
            });
            if matches!(
                engine.session_status("m"),
                Some(SessionStatus::Connected(_))
            ) {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        assert!(matches!(
            engine.session_status("m"),
            Some(SessionStatus::Connected(_))
        ));
        Server {
            engine,
            _lock: lock,
            _rt: rt,
        }
    }

    pub fn run(engine: &Engine, text: &str) -> QueryId {
        let q = engine.run_doc(&SessionKey::meta("m"), None, text.into(), 500);
        let status = settled(engine, q);
        assert!(
            !matches!(status, QueryStatus::Failed(_)),
            "{text}: {status:?}"
        );
        q
    }
}

#[test]
fn run_doc_pages_and_fetch_all() {
    sqlr_testkit::skip_container_tests!();
    let server = mongo::start();
    let engine = &server.engine;
    let docs: Vec<String> = (0..1_200)
        .map(|i| format!("{{n: {i}, cat: \"c{}\"}}", i % 3))
        .collect();
    mongo::run(
        engine,
        &format!("db.items.insertMany([{}])", docs.join(",")),
    );
    let q = mongo::run(engine, "db.items.find({}).sort({n: 1})");
    assert_eq!(count(engine, q), 500);
    assert_eq!(
        engine.doc_result(q).unwrap().read().status,
        QueryStatus::Ready { more: true }
    );
    engine.fetch_more_docs(q);
    assert!(wait_until(TIMEOUT * 4, || count(engine, q) == 1_000));
    engine.fetch_all_docs(q);
    assert!(wait_until(TIMEOUT * 4, || engine
        .doc_result(q)
        .is_some_and(
            |r| r.read().status == QueryStatus::Ready { more: false }
        )));
    {
        let r = engine.doc_result(q).unwrap();
        let r = r.read();
        assert_eq!(r.len(), 1_200);
        assert_eq!(
            r.doc(1_199).unwrap().get("n"),
            Some(&DocValue::Int32(1_199))
        );
    }
    // A tabular result (distinct) is an ordinary rows result.
    let d = mongo::run(engine, "db.items.distinct(\"cat\")");
    assert!(engine.doc_result(d).unwrap().read().tabular);
    assert!(wait_until(TIMEOUT, || engine.result(d).is_some_and(|r| r
        .read()
        .rows
        .row_count()
        == 3)));
    // A count is a value.
    let c = mongo::run(engine, "db.items.countDocuments({cat: \"c0\"})");
    assert_eq!(count(engine, c), 1);
}

#[test]
fn edit_document_preserves_types() {
    sqlr_testkit::skip_container_tests!();
    let server = mongo::start();
    let engine = &server.engine;
    let oid = new_object_id();
    let original = doc(vec![
        ("_id", DocValue::ObjectId(oid)),
        ("price", DocValue::Decimal128("1.10".into())),
        ("born", DocValue::DateTime(-86_400_000)),
        ("far", DocValue::DateTime(253_402_300_800_000 + 1)),
        (
            "uuid",
            DocValue::Binary {
                subtype: 4,
                bytes: (0..16).collect(),
            },
        ),
        (
            "custom",
            DocValue::Binary {
                subtype: 0x80,
                bytes: vec![0, 255],
            },
        ),
        ("small_long", DocValue::Int64(5)),
        ("int", DocValue::Int32(7)),
        ("whole", DocValue::Double(2.0)),
        (
            "nested",
            DocValue::Array(vec![DocValue::Array(vec![doc(vec![(
                "a.b",
                DocValue::Int32(1),
            )])])]),
        ),
        ("name", DocValue::String("before".into())),
    ]);
    let key = SessionKey::meta("m");
    let target = DocTarget {
        database: None,
        collection: "typed".into(),
    };
    let inserted = engine.save_doc(&key, target.clone(), None, original.clone());
    assert!(
        matches!(
            job(engine, inserted),
            JobOutcome::DocsWritten { written: 1, .. }
        ),
        "insert"
    );
    let q = mongo::run(engine, "db.typed.find({})");
    let shown = engine.doc_result(q).unwrap().read().doc(0).unwrap();
    assert_eq!(shown, original);
    // View → edit → save: the JSON view's text, with one field changed.
    let text = shown
        .to_relaxed_json_pretty()
        .replace("\"before\"", "\"after\"");
    let edited = DocValue::from_relaxed_json(&text).unwrap();
    let saved = engine.save_doc(&key, target, Some(DocValue::ObjectId(oid)), edited);
    assert!(matches!(
        job(engine, saved),
        JobOutcome::DocsWritten { written: 1, .. }
    ));
    let q = mongo::run(engine, "db.typed.find({})");
    let after = engine.doc_result(q).unwrap().read().doc(0).unwrap();
    let DocValue::Document(mut expected) = original else {
        unreachable!()
    };
    expected.last_mut().unwrap().1 = DocValue::String("after".into());
    assert_eq!(after, DocValue::Document(expected), "every type survived");
    // A document read back with a symbol (shown as `{"$symbol": …}`) is never saved: the
    // engine refuses it before the server sees it.
    let symbol = doc(vec![
        ("_id", DocValue::ObjectId(oid)),
        ("s", doc(vec![("$symbol", DocValue::String("x".into()))])),
    ]);
    let refused = engine.save_doc(
        &key,
        DocTarget {
            database: None,
            collection: "typed".into(),
        },
        Some(DocValue::ObjectId(oid)),
        symbol,
    );
    let JobOutcome::Failed(e) = job(engine, refused) else {
        panic!("a symbol must be refused")
    };
    assert!(e.message.contains("a symbol"), "{}", e.message);
    let q = mongo::run(engine, "db.typed.find({})");
    assert_eq!(
        engine
            .doc_result(q)
            .unwrap()
            .read()
            .doc(0)
            .unwrap()
            .get("name"),
        Some(&DocValue::String("after".into())),
        "unchanged"
    );
}

#[test]
fn delete_docs_confirms_count() {
    sqlr_testkit::skip_container_tests!();
    let server = mongo::start();
    let engine = &server.engine;
    mongo::run(engine, "db.del.insertMany([{_id: 1}, {_id: 2}, {_id: 3}])");
    let target = DocTarget {
        database: None,
        collection: "del".into(),
    };
    let id = engine.delete_docs(
        &SessionKey::meta("m"),
        target,
        vec![DocValue::Int32(1), DocValue::Int32(3), DocValue::Int32(9)],
    );
    assert_eq!(
        job(engine, id),
        JobOutcome::DocsWritten {
            requested: 3,
            written: 2,
            id: None
        }
    );
    let q = mongo::run(engine, "db.del.find({})");
    assert_eq!(
        docs_of(&engine.doc_result(q).unwrap().read()),
        [doc(vec![("_id", DocValue::Int32(2))])]
    );
}

// ----- Elasticsearch -----

#[test]
fn es_doc_edit_by_id_and_index() {
    sqlr_testkit::skip_container_tests!();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let (host, port) = support::endpoint("ELASTIC");
    let _lock = sqlr_testkit::TestLock::acquire("elasticsearch");
    rt.block_on(support::reset_elastic(&host, port));
    let mut drivers = DriverRegistry::default();
    drivers.register(sqlr_driver_elastic::ElasticDriver::new());
    let engine = Engine::new(drivers, no_repaint());
    let cfg = ConnectionConfig {
        values: [
            ("host".to_string(), host),
            ("port".to_string(), port.to_string()),
        ]
        .into(),
        ..config("e", "elasticsearch")
    };
    let key = SessionKey::meta("e");
    let run = |text: &str| -> Option<QueryId> {
        let q = engine.run_doc(&key, None, text.into(), 100);
        match settled(&engine, q) {
            QueryStatus::Failed(_) => None,
            _ => Some(q),
        }
    };
    // The node takes about 20 s to start.
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        engine.connect(cfg.clone(), Secrets::default(), dev());
        wait_until(Duration::from_secs(20), || {
            !matches!(engine.session_status("e"), Some(SessionStatus::Connecting))
        });
        if matches!(
            engine.session_status("e"),
            Some(SessionStatus::Connected(_))
        ) && run("GET _cluster/health?wait_for_status=yellow&timeout=60s").is_some()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "elasticsearch never started"
        );
        std::thread::sleep(Duration::from_secs(2));
    }
    for (index, id, n) in [("logs-a", "1", 1), ("logs-b", "1", 2)] {
        run(&format!(
            "PUT {index}/_doc/{id}?refresh=true\n{{\"n\": {n}, \"msg\": \"m{n}\"}}"
        ))
        .expect("seed");
    }
    run("POST _aliases\n{\"actions\": [{\"add\": {\"index\": \"logs-*\", \"alias\": \"logs\"}}]}")
        .expect("alias");
    let q = run("GET logs/_search\n{\"sort\": [{\"n\": \"asc\"}]}").expect("search");
    let (hit_b, target) = {
        let r = engine.doc_result(q).unwrap();
        let r = r.read();
        assert_eq!(r.len(), 2);
        assert!(r.read_only.is_none(), "{:?}", r.read_only);
        (r.doc(1).unwrap(), r.target.clone().unwrap())
    };
    assert_eq!(target.collection, "logs", "the search's own expression");
    assert_eq!(
        hit_b.get("_index"),
        Some(&DocValue::String("logs-b".into()))
    );
    // Saved to the hit's concrete index (as the app passes it); through the alias, or with
    // `_index` edited in the text, the save is refused.
    let concrete = DocTarget {
        database: None,
        collection: "logs-b".into(),
    };
    let through_alias = engine.save_doc(
        &key,
        target,
        Some(DocValue::String("1".into())),
        hit_b.clone(),
    );
    let JobOutcome::Failed(e) = job(&engine, through_alias) else {
        panic!("an alias must be refused")
    };
    assert!(
        e.message.contains("`_index` cannot be changed"),
        "{}",
        e.message
    );
    let DocValue::Document(mut moved) = hit_b.clone() else {
        unreachable!()
    };
    moved[0].1 = DocValue::String("logs-a".into());
    let edited_index = engine.save_doc(
        &key,
        concrete.clone(),
        Some(DocValue::String("1".into())),
        DocValue::Document(moved),
    );
    assert!(matches!(job(&engine, edited_index), JobOutcome::Failed(_)));
    let DocValue::Document(mut fields) = hit_b else {
        unreachable!()
    };
    for (k, v) in &mut fields {
        if k == "msg" {
            *v = DocValue::String("edited".into());
        }
    }
    let saved = engine.save_doc(
        &key,
        concrete,
        Some(DocValue::String("1".into())),
        DocValue::Document(fields),
    );
    assert!(matches!(
        job(&engine, saved),
        JobOutcome::DocsWritten { written: 1, .. }
    ));
    let a = run("GET logs-a/_doc/1").unwrap();
    let b = run("GET logs-b/_doc/1").unwrap();
    let source = |q: QueryId| {
        let r = engine.doc_result(q).unwrap();
        let d = r.read().doc(0).unwrap();
        d.get("_source").and_then(|s| s.get("msg")).cloned()
    };
    assert_eq!(
        source(a),
        Some(DocValue::String("m1".into())),
        "logs-a untouched"
    );
    assert_eq!(source(b), Some(DocValue::String("edited".into())));
    // A delete never goes to a pattern.
    let refused = engine.delete_docs(
        &key,
        DocTarget {
            database: None,
            collection: "logs-*".into(),
        },
        vec![DocValue::String("1".into())],
    );
    let JobOutcome::Failed(e) = job(&engine, refused) else {
        panic!("a pattern must be refused")
    };
    assert!(e.message.contains("concrete index"), "{}", e.message);
    let deleted = engine.delete_docs(
        &key,
        DocTarget {
            database: None,
            collection: "logs-a".into(),
        },
        vec![DocValue::String("1".into())],
    );
    assert!(matches!(
        job(&engine, deleted),
        JobOutcome::DocsWritten { written: 1, .. }
    ));
    drop(engine);
}

#[test]
fn mongodb_database_tools_run_through_the_sidecar() {
    sqlr_testkit::skip_container_tests!("MONGO");
    let server = mongo::start();
    let engine = &server.engine;
    let id = engine.doc_admin(
        &SessionKey::meta("m"),
        Some("test".into()),
        "db.tools.createIndex({n:1}, {name:'n_1'})".into(),
    );
    assert_eq!(job(engine, id), JobOutcome::Done);
    let query = mongo::run(engine, "db.tools.getIndexes()");
    let result = engine.doc_result(query).unwrap();
    let result = result.read();
    assert!(
        (0..result.len())
            .filter_map(|i| result.doc(i))
            .any(|d| d.get("name") == Some(&DocValue::String("n_1".into())))
    );
    drop(result);
    let id = engine.doc_admin(
        &SessionKey::meta("m"),
        Some("test".into()),
        "db.tools.dropIndex('n_1')".into(),
    );
    assert_eq!(job(engine, id), JobOutcome::Done);
}
