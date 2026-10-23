use sqlr_core::{
    ConnectionConfig, DocExecution, DocTarget, DocValue, Driver, ErrorKind, Family, QueryLanguage,
    Secrets, StatementClass,
};
use sqlr_testkit::{FakeDocDriver, FakeDocScript};

fn cfg() -> ConnectionConfig {
    ConnectionConfig {
        id: "1".into(),
        name: "t".into(),
        driver: "fakedoc".into(),
        group: None,
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    }
}

fn doc(i: i32) -> DocValue {
    DocValue::Document(vec![
        ("_id".into(), DocValue::Int32(i)),
        ("n".into(), DocValue::String(format!("d{i}"))),
    ])
}

#[tokio::test]
async fn fake_document_connection_pages_and_edits() {
    let driver = FakeDocDriver::new(FakeDocScript {
        docs: (0..7).map(doc).collect(),
        ..Default::default()
    });
    assert_eq!(driver.manifest().family, Family::Document);
    let conn = driver.connect(&cfg(), &Secrets::default()).await.unwrap();
    let d = conn.document().expect("a document connection");
    assert_eq!(d.language(), QueryLanguage::MongoShell);

    let DocExecution::Docs(mut stream) = d.run(Some("db1"), "db.c.find()").await.unwrap() else {
        panic!("expected documents")
    };
    let first = stream.next_chunk(5).await.unwrap().unwrap();
    assert_eq!(first.len(), 5);
    assert_eq!(first.doc(0), doc(0));
    assert_eq!(stream.next_chunk(5).await.unwrap().unwrap().len(), 2);
    assert!(stream.next_chunk(5).await.unwrap().is_none());
    stream.close().await.unwrap();
    assert_eq!(
        driver.runs(),
        [(Some("db1".to_string()), "db.c.find()".to_string())]
    );

    let target = DocTarget {
        database: None,
        collection: "c".into(),
    };
    assert_eq!(
        d.replace_doc(&target, &DocValue::Int32(1), &doc(10))
            .await
            .unwrap(),
        1
    );
    assert_eq!(d.delete_doc(&target, &DocValue::Int32(2)).await.unwrap(), 1);
    assert_eq!(
        d.delete_doc(&target, &DocValue::Int32(99)).await.unwrap(),
        0
    );
    let id = d.insert_doc(&target, &doc(42)).await.unwrap();
    assert_eq!(id, DocValue::Int32(42));
    assert_eq!(driver.docs().len(), 7);
    assert_eq!(driver.docs()[1], doc(10));

    assert!(matches!(
        d.classify("db.c.drop()"),
        StatementClass::Destructive(_)
    ));
    assert_eq!(d.classify("db.c.find()"), StatementClass::Read);
    assert_eq!(d.open_target_text(&target, 50), "db.c.find({}).limit(50)");

    d.set_read_only(true);
    let e = match d.run(None, "db.c.insertOne({})").await {
        Err(e) => e,
        Ok(_) => panic!("read-only must refuse writes"),
    };
    assert_eq!(e.kind, ErrorKind::Permission);
    assert!(d.delete_doc(&target, &DocValue::Int32(1)).await.is_err());
    assert!(d.run(None, "db.c.find()").await.is_ok());
}
