//! The identical Document checks in process and over the sidecar pipes.
use sqlr_core::{ConnectionConfig, DocTarget, Driver, Secrets};
use sqlr_driver_mongo::MongoDriver;
use sqlr_driver_tests::{DocumentTarget, document_suite};
use sqlr_sidecar::{SidecarDriver, SidecarSpec};
use std::sync::Arc;

fn target(driver: Arc<dyn Driver>, database: &str) -> DocumentTarget {
    let (host, port) = sqlr_testkit::service("MONGO");
    let config = ConnectionConfig {
        id: "m".into(),
        name: "MongoDB conformance".into(),
        driver: "mongodb".into(),
        group: None,
        color: None,
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
        values: [
            ("host".into(), host),
            ("port".into(), port.to_string()),
            ("database".into(), database.into()),
        ]
        .into(),
    };
    let documents = (1..=25)
        .map(|n| format!("{{_id:{n}, n:{n}}}"))
        .collect::<Vec<_>>()
        .join(",");
    DocumentTarget {
        connect: Box::new(move || { let driver = driver.clone(); let config = config.clone();
            Box::pin(async move { driver.connect(&config, &Secrets::default()).await }) }),
        target: DocTarget { database: Some(database.into()), collection: "items".into() },
        view: DocTarget { database: Some(database.into()), collection: "view".into() },
        setup: vec![format!("db.items.insertMany([{documents}])"),
            "db.runCommand({create: 'view', viewOn: 'items', pipeline: []})".into()],
        docs: "db.items.find({}).sort({_id:1})".into(),
        rows: "db.items.distinct('n')".into(),
        write: "db.writes.insertOne({_id: 42})".into(),
        value: "db.items.countDocuments({})".into(),
        slow: Some("db.items.countDocuments({$expr: {$function: {body: 'function() { sleep(100); return true; }', args: [], lang: 'js'}}})".into()),
    }
}

#[tokio::test]
async fn document_conformance_in_process_and_sidecar_match() {
    sqlr_testkit::skip_container_tests!("MONGO");
    let _lock = sqlr_testkit::TestLock::acquire("mongo");
    let database = sqlr_testkit::unique_name("conformance");
    let driver = MongoDriver::new();
    let direct = document_suite(&target(driver.clone(), &format!("{database}_direct"))).await;
    assert!(direct.failed.is_empty(), "in process: {direct}");
    let sidecar = Arc::new(
        SidecarDriver::probe(SidecarSpec::new(
            "mongodb",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_BIN_EXE_sqlr-driver-mongodb"),
        ))
        .await
        .unwrap()
        .0,
    );
    let remote = document_suite(&target(sidecar, &format!("{database}_remote"))).await;
    assert!(remote.failed.is_empty(), "sidecar: {remote}");
    assert_eq!(direct, remote);
    // Clean up using the same public Document interface.
    for suffix in ["direct", "remote"] {
        let t = target(driver.clone(), &format!("{database}_{suffix}"));
        let conn = (t.connect)().await.unwrap();
        conn.document()
            .unwrap()
            .run(None, "db.runCommand({dropDatabase:1})")
            .await
            .unwrap();
    }
}
