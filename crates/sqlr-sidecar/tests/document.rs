//! Document requests over pipes, using the same public driver interface as the app.
use sqlr_core::{ConnectionConfig, DocValue, ErrorKind, Secrets};
use sqlr_sidecar::frame::{CONTROL, DOC_CHUNK, read_frame, write_frame};
use sqlr_sidecar::messages::{decode, encode};
use sqlr_sidecar::{PROTOCOL, Request, Response, serve_io};
use sqlr_testkit::{FakeDocDriver, FakeDocScript};

#[tokio::test]
async fn document_stream_pages_use_document_frames() {
    let driver = FakeDocDriver::new(FakeDocScript {
        docs: vec![DocValue::ObjectId([7; 12]), DocValue::Int64(42)],
        ..Default::default()
    });
    let (mut client, server) = tokio::io::duplex(4096);
    let (input, output) = tokio::io::split(server);
    let task = tokio::spawn(serve_io(driver, PROTOCOL.into(), input, output));
    let cfg = ConnectionConfig {
        id: "c".into(),
        name: "c".into(),
        driver: "fakedoc".into(),
        group: None,
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    };
    for request in [
        Request::Hello {
            protocol: "1.1.0".into(),
            sqlr_version: "test".into(),
        },
        Request::Connect {
            conn: 1,
            config: cfg,
            secrets: Secrets::default(),
        },
    ] {
        write_frame(&mut client, CONTROL, 1, &encode(&request))
            .await
            .unwrap();
        let frame = read_frame(&mut client).await.unwrap().unwrap();
        assert!(!matches!(
            decode::<Response>(&frame.payload).unwrap(),
            Response::Error(_)
        ));
    }
    write_frame(
        &mut client,
        CONTROL,
        2,
        &encode(&Request::DocRun {
            conn: 1,
            database: Some("db".into()),
            text: "db.c.find({})".into(),
        }),
    )
    .await
    .unwrap();
    let frame = read_frame(&mut client).await.unwrap().unwrap();
    let Response::Docs { stream } = decode(&frame.payload).unwrap() else {
        panic!("expected documents")
    };
    write_frame(
        &mut client,
        CONTROL,
        3,
        &encode(&Request::DocFetch {
            conn: 1,
            stream,
            max: 1,
        }),
    )
    .await
    .unwrap();
    let frame = read_frame(&mut client).await.unwrap().unwrap();
    assert_eq!((frame.ty, frame.id), (DOC_CHUNK, 3));
    assert_eq!(
        sqlr_core::decode_doc_chunk(&frame.payload).unwrap().doc(0),
        DocValue::ObjectId([7; 12])
    );
    let frame = read_frame(&mut client).await.unwrap().unwrap();
    assert!(matches!(
        decode::<Response>(&frame.payload).unwrap(),
        Response::DocFetchDone {
            exhausted: false,
            ..
        }
    ));
    write_frame(
        &mut client,
        CONTROL,
        4,
        &encode(&Request::DocSetReadOnly { conn: 1, on: true }),
    )
    .await
    .unwrap();
    let _ = read_frame(&mut client).await.unwrap();
    write_frame(
        &mut client,
        CONTROL,
        5,
        &encode(&Request::DocRun {
            conn: 1,
            database: None,
            text: "db.c.insertMany([{_id: 2}])".into(),
        }),
    )
    .await
    .unwrap();
    let frame = read_frame(&mut client).await.unwrap().unwrap();
    assert!(
        matches!(decode::<Response>(&frame.payload).unwrap(), Response::Error(e) if e.kind == ErrorKind::Permission)
    );
    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn document_client_supports_editing_targets_and_local_language() {
    use sqlr_core::{DocExecution, DocTarget, Driver, NodePath, QueryLanguage, StatementClass};
    use sqlr_sidecar::{SidecarDriver, SidecarSpec};
    let spec = SidecarSpec::new(
        "fakedoc",
        "1.0.0",
        sqlr_testkit::workspace_binary("examples/document_driver"),
    );
    let (driver, _) = SidecarDriver::probe(spec).await.unwrap();
    let cfg = ConnectionConfig {
        id: "c".into(),
        name: "c".into(),
        driver: "fakedoc".into(),
        group: None,
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    };
    let conn = driver.connect(&cfg, &Secrets::default()).await.unwrap();
    assert!(conn.relational().is_none());
    let doc = conn
        .document()
        .expect("Document manifest exposes Document interface");
    assert_eq!(doc.language(), QueryLanguage::MongoShell);
    assert_eq!(doc.classify("db.c.find({})"), StatementClass::Read);
    assert_eq!(doc.classify("db.c.insertOne({x:1})"), StatementClass::Write);
    assert_eq!(doc.split("db.c.find({}); db.c.countDocuments({})").len(), 2);
    let nodes = conn
        .browse(&NodePath(vec!["db".into(), "collections".into()]))
        .await
        .unwrap();
    let target = doc.node_target(&nodes[0].path).unwrap();
    assert_eq!(target.collection, "a.b");
    assert_eq!(
        doc.open_target_text(&target, 20),
        "db.getCollection(\"a.b\").find({}).limit(20)"
    );
    assert!(doc.node_target(&NodePath::root()).is_none());
    assert!(
        doc.is_view(&DocTarget {
            collection: "view".into(),
            ..target.clone()
        })
        .await
        .unwrap()
    );
    assert!(!doc.is_view(&target).await.unwrap());
    assert_eq!(
        doc.indexes(&target).await.unwrap()[0].get("name"),
        Some(&DocValue::String("_id_".into()))
    );
    let DocExecution::Docs(mut stream) = doc.run(Some("db"), "db.c.find({})").await.unwrap() else {
        panic!("docs")
    };
    let original = stream.next_chunk(1).await.unwrap().unwrap().doc(0);
    assert!(stream.next_chunk(1).await.unwrap().is_none());
    stream.close().await.unwrap();
    let id = original.get("_id").unwrap();
    let replacement = DocValue::Document(vec![
        ("_id".into(), id.clone()),
        ("n".into(), DocValue::Int64(123)),
    ]);
    assert_eq!(doc.replace_doc(&target, id, &replacement).await.unwrap(), 1);
    assert_eq!(doc.delete_doc(&target, id).await.unwrap(), 1);
    assert_eq!(doc.insert_doc(&target, &replacement).await.unwrap(), *id);
    doc.set_read_only(true);
    // No yield or round trip between toggle and write: wire order enforces it.
    assert_eq!(
        doc.delete_doc(&target, id).await.unwrap_err().kind,
        ErrorKind::Permission
    );
    doc.set_read_only(false);
    assert_eq!(doc.delete_doc(&target, id).await.unwrap(), 1);
    driver.restart();
    assert_eq!(conn.ping().await.unwrap_err().kind, ErrorKind::Protocol);
}

#[tokio::test]
async fn document_driver_requires_a_1_1_welcome() {
    use sqlr_sidecar::{SidecarDriver, SidecarSpec};
    let mut spec = SidecarSpec::new(
        "fakedoc",
        "1.0.0",
        sqlr_testkit::workspace_binary("examples/document_driver"),
    );
    spec.env.push(("DOC_PROTOCOL".into(), "1.0.0".into()));
    let error = SidecarDriver::probe(spec).await.err().unwrap();
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(error.message.contains("needs 1.1"), "{error}");
}

#[test]
fn document_request_debug_hides_text_ids_and_contents() {
    use sqlr_sidecar::messages::DocValues;
    let target = sqlr_core::DocTarget {
        database: None,
        collection: "c".into(),
    };
    let values = DocValues::new(&[DocValue::String("secret marker".into())]);
    for request in [
        Request::DocRun {
            conn: 1,
            database: None,
            text: "secret marker".into(),
        },
        Request::DocInsert {
            conn: 1,
            target: target.clone(),
            values: values.clone(),
        },
        Request::DocReplace {
            conn: 1,
            target: target.clone(),
            values: values.clone(),
        },
        Request::DocDelete {
            conn: 1,
            target,
            values,
        },
    ] {
        for debug in [format!("{request:?}"), format!("{request:#?}")] {
            assert!(!debug.contains("secret marker"));
        }
        let decoded: Request = decode(&encode(&request)).unwrap();
        assert_eq!(encode(&decoded), encode(&request));
    }
    assert!(DocValues(vec![1, 1, 2, 255, 0]).decode().is_err());
    assert!(DocValues::new(&[DocValue::Null]).exactly::<2>().is_err());
}

#[tokio::test]
async fn document_driver_refuses_a_1_0_hello() {
    let (mut client, server) = tokio::io::duplex(4096);
    let (input, output) = tokio::io::split(server);
    let task = tokio::spawn(serve_io(
        FakeDocDriver::new(FakeDocScript::default()),
        PROTOCOL.into(),
        input,
        output,
    ));
    write_frame(
        &mut client,
        CONTROL,
        1,
        &encode(&Request::Hello {
            protocol: "1.0.0".into(),
            sqlr_version: "old".into(),
        }),
    )
    .await
    .unwrap();
    let frame = read_frame(&mut client).await.unwrap().unwrap();
    assert!(
        matches!(decode::<Response>(&frame.payload).unwrap(), Response::Error(e) if e.kind == ErrorKind::Protocol)
    );
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn oversized_document_pages_split_and_single_documents_fail() {
    async fn fetch(docs: Vec<DocValue>) -> (Vec<usize>, Response) {
        let (mut client, server) = tokio::io::duplex(1 << 20);
        let (input, output) = tokio::io::split(server);
        let driver = FakeDocDriver::new(FakeDocScript {
            docs,
            ..Default::default()
        });
        let task = tokio::spawn(serve_io(driver, PROTOCOL.into(), input, output));
        let cfg = ConnectionConfig {
            id: "c".into(),
            name: "c".into(),
            driver: "fakedoc".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        write_frame(
            &mut client,
            CONTROL,
            1,
            &encode(&Request::Connect {
                conn: 1,
                config: cfg,
                secrets: Secrets::default(),
            }),
        )
        .await
        .unwrap();
        read_frame(&mut client).await.unwrap();
        write_frame(
            &mut client,
            CONTROL,
            2,
            &encode(&Request::DocRun {
                conn: 1,
                database: None,
                text: "db.c.find({})".into(),
            }),
        )
        .await
        .unwrap();
        let frame = read_frame(&mut client).await.unwrap().unwrap();
        let Response::Docs { stream } = decode(&frame.payload).unwrap() else {
            panic!("documents")
        };
        write_frame(
            &mut client,
            CONTROL,
            3,
            &encode(&Request::DocFetch {
                conn: 1,
                stream,
                max: 10,
            }),
        )
        .await
        .unwrap();
        let mut pages = Vec::new();
        let response = loop {
            let frame = read_frame(&mut client).await.unwrap().unwrap();
            assert_eq!(frame.id, 3);
            if frame.ty == DOC_CHUNK {
                pages.push(sqlr_core::decode_doc_chunk(&frame.payload).unwrap().len());
            } else {
                break decode(&frame.payload).unwrap();
            }
        };
        drop(client);
        task.await.unwrap().unwrap();
        (pages, response)
    }
    let (sizes, response) = fetch(vec![
        DocValue::String("a".repeat(34 * 1024 * 1024)),
        DocValue::String("b".repeat(34 * 1024 * 1024)),
    ])
    .await;
    assert_eq!(sizes, [1, 1]);
    assert!(matches!(
        response,
        Response::DocFetchDone {
            exhausted: false,
            ..
        }
    ));
    let (sizes, response) = fetch(vec![DocValue::String("x".repeat(65 * 1024 * 1024))]).await;
    assert!(sizes.is_empty());
    assert!(
        matches!(response, Response::Error(e) if e.message.contains("a document is larger than the 64 MiB"))
    );
}

#[tokio::test]
async fn split_document_frames_respect_a_smaller_next_page_request() {
    use sqlr_core::{DocExecution, Driver};
    use sqlr_sidecar::{SidecarDriver, SidecarSpec};
    let mut spec = SidecarSpec::new(
        "fakedoc",
        "1.0.0",
        sqlr_testkit::workspace_binary("examples/document_driver"),
    );
    spec.env.push(("DOC_LARGE_PAGE".into(), "1".into()));
    let driver = SidecarDriver::probe(spec).await.unwrap().0;
    let cfg = ConnectionConfig {
        id: "c".into(),
        name: "c".into(),
        driver: "fakedoc".into(),
        group: None,
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    };
    let conn = driver.connect(&cfg, &Secrets::default()).await.unwrap();
    let DocExecution::Docs(mut stream) = conn
        .document()
        .unwrap()
        .run(None, "db.c.find({})")
        .await
        .unwrap()
    else {
        panic!("docs")
    };
    assert_eq!(stream.next_chunk(4).await.unwrap().unwrap().len(), 2);
    assert_eq!(stream.next_chunk(1).await.unwrap().unwrap().len(), 1);
    assert_eq!(stream.next_chunk(1).await.unwrap().unwrap().len(), 1);
    assert!(stream.next_chunk(1).await.unwrap().is_none());
}

#[tokio::test]
async fn read_only_toggles_fence_buffered_document_writes_in_wire_order() {
    let (mut client, server) = tokio::io::duplex(1 << 20);
    let (input, output) = tokio::io::split(server);
    let task = tokio::spawn(serve_io(
        FakeDocDriver::new(FakeDocScript::default()),
        PROTOCOL.into(),
        input,
        output,
    ));
    let cfg = ConnectionConfig {
        id: "c".into(),
        name: "c".into(),
        driver: "fakedoc".into(),
        group: None,
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    };
    write_frame(
        &mut client,
        CONTROL,
        1,
        &encode(&Request::Connect {
            conn: 1,
            config: cfg,
            secrets: Secrets::default(),
        }),
    )
    .await
    .unwrap();
    read_frame(&mut client).await.unwrap();
    // The whole batch is buffered before the server polls any of the writes.
    for (id, request) in [
        (2, Request::DocSetReadOnly { conn: 1, on: true }),
        (
            3,
            Request::DocInsert {
                conn: 1,
                target: sqlr_core::DocTarget {
                    database: None,
                    collection: "c".into(),
                },
                values: sqlr_sidecar::messages::DocValues::new(&[DocValue::Document(vec![(
                    "_id".into(),
                    DocValue::Int32(1),
                )])]),
            },
        ),
        (4, Request::DocSetReadOnly { conn: 1, on: false }),
    ] {
        write_frame(&mut client, CONTROL, id, &encode(&request))
            .await
            .unwrap();
    }
    let mut insert = None;
    for _ in 0..3 {
        let frame = read_frame(&mut client).await.unwrap().unwrap();
        if frame.id == 3 {
            insert = Some(decode::<Response>(&frame.payload).unwrap());
        }
    }
    assert!(matches!(insert, Some(Response::Error(e)) if e.kind == ErrorKind::Permission));
    drop(client);
    task.await.unwrap().unwrap();
}
