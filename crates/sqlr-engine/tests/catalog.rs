use std::sync::Arc;
use std::time::Duration;

use sqlr_core::{
    CatalogColumn, CatalogNeed, CatalogRelation, DriverError, ErrorKind, Node, NodeKind, NodePath,
    RelationKind, SchemaCatalog, Secrets,
};
use sqlr_engine::{ConnCatalog, DriverRegistry, Engine, LoadState, QueryStatus, SessionStatus};
use sqlr_store::Db;
use sqlr_testkit::{FakeDriver, FakeScript};

mod support;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn schema_node(name: &str) -> Node {
    Node {
        path: NodePath(vec![name.to_string()]),
        name: name.to_string(),
        kind: NodeKind::Schema,
        has_children: true,
        detail: None,
    }
}

fn users(schema: &str) -> SchemaCatalog {
    SchemaCatalog {
        schema: schema.into(),
        relations: vec![CatalogRelation {
            name: "users".into(),
            kind: RelationKind::Table,
            columns: vec![CatalogColumn {
                name: "id".into(),
                type_name: "int4".into(),
            }],
        }],
    }
}

/// Schemas `s` and `t` at the tree root; only `s` has a catalog.
fn script() -> FakeScript {
    FakeScript {
        rows: 1,
        tree: vec![schema_node("s"), schema_node("t")],
        catalog: vec![users("s")],
        ..Default::default()
    }
}

fn engine_with(fake: Arc<FakeDriver>) -> Engine {
    let mut drivers = DriverRegistry::default();
    drivers.register(fake);
    Engine::new(drivers, no_repaint())
}

fn engine_on(fake: Arc<FakeDriver>, db: Arc<Db>) -> Engine {
    let mut drivers = DriverRegistry::default();
    drivers.register(fake);
    Engine::with_db(drivers, no_repaint(), db)
}

fn wait_connected(engine: &Engine) {
    assert!(
        wait_until(TIMEOUT, || matches!(
            engine.session_status("c"),
            Some(SessionStatus::Connected(_))
        )),
        "session never connected: {:?}",
        engine.session_status("c")
    );
}

fn connected(fake: Arc<FakeDriver>) -> Engine {
    let engine = engine_with(fake);
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    wait_connected(&engine);
    engine
}

fn catalog(engine: &Engine) -> Arc<ConnCatalog> {
    engine.catalog("c").unwrap_or_default()
}

fn schema(engine: &Engine, s: &str) -> Option<LoadState<Arc<SchemaCatalog>>> {
    catalog(engine).schema.get(s).cloned()
}

fn schema_ready(engine: &Engine, s: &str) -> bool {
    matches!(schema(engine, s), Some(LoadState::Ready(_)))
}

/// Loads schema `s` on the connected session "c" and waits until it is `Ready`.
fn load_s(engine: &Engine) {
    engine.load_catalog("c", CatalogNeed::Schema("s".into()));
    assert!(
        wait_until(TIMEOUT, || schema_ready(engine, "s")),
        "schema s: {:?}",
        schema(engine, "s")
    );
}

fn settle(engine: &Engine, sql: &str) {
    let q = engine.execute("c", sql.into(), 500);
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
}

#[test]
fn loads_schema_list_and_schema() {
    let fake = FakeDriver::new(script());
    let engine = connected(fake.clone());

    engine.load_catalog("c", CatalogNeed::Schemas);
    assert!(wait_until(TIMEOUT, || matches!(
        catalog(&engine).schemas,
        Some(LoadState::Ready(_))
    )));
    assert_eq!(
        catalog(&engine).schemas,
        Some(LoadState::Ready(vec!["s".to_string(), "t".to_string()]))
    );

    load_s(&engine);
    assert_eq!(
        schema(&engine, "s"),
        Some(LoadState::Ready(Arc::new(users("s"))))
    );
    assert_eq!(fake.catalog_calls(), 1);
    assert!(!catalog(&engine).from_cache);

    // A loaded schema is not requested again.
    let revision = engine.catalog_revision();
    engine.load_catalog("c", CatalogNeed::Schema("s".into()));
    assert_eq!(engine.catalog_revision(), revision);
    // Sync point: a later load has come back, so a stray reload of `s` would show by now.
    engine.load_catalog("c", CatalogNeed::Schema("t".into()));
    assert!(wait_until(TIMEOUT, || matches!(
        schema(&engine, "t"),
        Some(LoadState::Failed(_))
    )));
    assert_eq!(fake.catalog_calls(), 2);

    let all = engine.all_catalogs();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].0, "c");
}

#[test]
fn load_before_connected_waits() {
    let engine = engine_with(FakeDriver::new(script()));
    // Issued in the same frame as connect: it waits for the connect attempt (ruling R4).
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    engine.load_catalog("c", CatalogNeed::Schema("s".into()));
    assert!(
        wait_until(TIMEOUT, || schema_ready(&engine, "s")),
        "schema s: {:?}",
        schema(&engine, "s")
    );
}

#[test]
fn load_without_session_is_ignored() {
    let fake = FakeDriver::new(script());
    let engine = engine_with(fake.clone());
    engine.load_catalog("x", CatalogNeed::Schemas);
    engine.load_catalog("x", CatalogNeed::Schema("s".into()));
    assert!(engine.catalog("x").is_none());
    assert!(engine.all_catalogs().is_empty());
    assert_eq!(engine.catalog_revision(), 0);
    assert_eq!(fake.catalog_calls(), 0);
}

#[test]
fn ddl_invalidates() {
    let engine = connected(FakeDriver::new(script()));
    load_s(&engine);

    let revision = engine.catalog_revision();
    settle(&engine, "select 1");
    assert!(
        schema_ready(&engine, "s"),
        "a plain query keeps the catalog"
    );
    assert_eq!(engine.catalog_revision(), revision);

    engine.execute("c", "CREATE TABLE z(i int)".into(), 500);
    assert!(
        wait_until(Duration::from_secs(1), || catalog(&engine)
            .schema
            .is_empty()),
        "DDL left the catalog: {:?}",
        engine.catalog("c")
    );
    assert!(engine.catalog_revision() > revision);
}

#[test]
fn root_refresh_invalidates() {
    let engine = connected(FakeDriver::new(script()));
    load_s(&engine);

    let revision = engine.catalog_revision();
    engine.refresh("c", NodePath(vec!["s".into()]));
    assert!(schema_ready(&engine, "s"), "a subtree refresh keeps it");
    assert_eq!(engine.catalog_revision(), revision);

    engine.refresh("c", NodePath::root());
    assert!(catalog(&engine).schema.is_empty());
    assert!(engine.catalog_revision() > revision);
}

#[test]
fn persisted_and_restored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");

    // Engine A loads `s`; dropping it flushes the write to the file.
    let fake_a = FakeDriver::new(script());
    let engine_a = engine_on(fake_a, Arc::new(Db::open(&path).unwrap()));
    engine_a.connect(config("c", "fake"), Secrets::default(), dev());
    wait_connected(&engine_a);
    load_s(&engine_a);
    drop(engine_a);

    // Engine B shows the cached catalog before any connect.
    let fake_b = FakeDriver::new(script());
    let engine_b = engine_on(fake_b.clone(), Arc::new(Db::open(&path).unwrap()));
    assert!(
        wait_until(TIMEOUT, || schema_ready(&engine_b, "s")),
        "restored catalog: {:?}",
        engine_b.catalog("c")
    );
    assert_eq!(
        schema(&engine_b, "s"),
        Some(LoadState::Ready(Arc::new(users("s"))))
    );
    assert!(catalog(&engine_b).from_cache);
    assert_eq!(fake_b.catalog_calls(), 0);

    // Connecting refreshes it from the server.
    engine_b.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(
        wait_until(TIMEOUT, || !catalog(&engine_b).from_cache),
        "catalog never refreshed: {:?}",
        engine_b.catalog("c")
    );
    assert!(fake_b.catalog_calls() >= 1);
    assert!(schema_ready(&engine_b, "s"));
}

#[test]
fn loads_catalog_of_another_database() {
    let schema_in = |db: &str, name: &str| Node {
        path: NodePath(vec![db.to_string(), name.to_string()]),
        ..schema_node(name)
    };
    let fake = FakeDriver::new(FakeScript {
        tree: vec![schema_node("x"), schema_in("main", "s")],
        schema_path: NodePath(vec!["main".into()]),
        own_database: Some("main".into()),
        databases: vec!["other".into()],
        catalog: vec![users("s"), users("x")],
        ..Default::default()
    });
    let engine = connected(fake.clone());
    assert_eq!(fake.open_connections(), 1);

    let scoped = |engine: &Engine, schema: &str| {
        engine
            .catalog_in("c", Some("other"))
            .and_then(|c| c.schema.get(schema).cloned())
    };
    engine.load_catalog_in("c", Some("other"), CatalogNeed::Schemas);
    engine.load_catalog_in("c", Some("other"), CatalogNeed::Schema("x".into()));
    assert!(
        wait_until(TIMEOUT, || {
            let Some(cat) = engine.catalog_in("c", Some("other")) else {
                return false;
            };
            cat.schemas == Some(LoadState::Ready(vec!["x".to_string()]))
                && scoped(&engine, "x") == Some(LoadState::Ready(Arc::new(users("x"))))
        }),
        "other database catalog: {:?}",
        engine.catalog_in("c", Some("other"))
    );
    // Reading another database opened a connection to it and left the own catalog alone.
    assert_eq!(fake.open_connections(), 2);
    assert!(engine.catalog("c").is_none());
    assert!(engine.all_catalogs().is_empty());
    assert_eq!(fake.catalog_calls(), 1);

    load_s(&engine);
    assert_eq!(
        schema(&engine, "s"),
        Some(LoadState::Ready(Arc::new(users("s"))))
    );
    assert_eq!(engine.all_catalogs().len(), 1);
}

#[test]
fn catalog_connection_error_fails_session() {
    let engine = connected(FakeDriver::new(FakeScript {
        catalog_error: Some(DriverError::new(ErrorKind::Connection, "server closed")),
        ..script()
    }));
    engine.load_catalog("c", CatalogNeed::Schema("s".into()));
    assert!(
        wait_until(TIMEOUT, || matches!(
            engine.session_status("c"),
            Some(SessionStatus::Failed(e)) if e.kind == ErrorKind::Connection
        )),
        "session: {:?}",
        engine.session_status("c")
    );
}

#[test]
fn schema_list_under_database_node() {
    // "Show all databases": the root lists databases and the connection's own schemas sit
    // under its database node.
    let database = |name: &str| Node {
        path: NodePath(vec![name.to_string()]),
        name: name.to_string(),
        kind: NodeKind::Database,
        has_children: true,
        detail: None,
    };
    let schema_in = |db: &str, name: &str| Node {
        path: NodePath(vec![db.to_string(), name.to_string()]),
        ..schema_node(name)
    };
    let engine = connected(FakeDriver::new(FakeScript {
        tree: vec![
            database("main"),
            database("other"),
            schema_in("main", "s"),
            schema_in("main", "t"),
            schema_in("other", "x"),
        ],
        schema_path: NodePath(vec!["main".into()]),
        ..script()
    }));
    engine.load_catalog("c", CatalogNeed::Schemas);
    assert!(wait_until(TIMEOUT, || matches!(
        catalog(&engine).schemas,
        Some(LoadState::Ready(_))
    )));
    assert_eq!(
        catalog(&engine).schemas,
        Some(LoadState::Ready(vec!["s".to_string(), "t".to_string()]))
    );
}

#[test]
fn reload_on_connect_keeps_stale_data_visible() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sqlr.db");
    let db = Db::open(&path).unwrap();
    db.save_schema_catalog("c", &users("s")).unwrap();
    drop(db);

    // The server's answer differs from the cache and is held back until released.
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut fresh = users("s");
    fresh.relations[0].name = "accounts".into();
    let fake = FakeDriver::new(FakeScript {
        catalog: vec![fresh.clone()],
        catalog_gate: Some(gate.clone()),
        ..script()
    });
    let engine = engine_on(fake.clone(), Arc::new(Db::open(&path).unwrap()));
    assert!(wait_until(TIMEOUT, || schema_ready(&engine, "s")));

    let revision = engine.catalog_revision();
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(
        wait_until(TIMEOUT, || fake.catalog_calls() == 1),
        "the cached schema is reloaded on connect"
    );
    // While the reload is in flight, the cached data stays visible.
    assert_eq!(
        schema(&engine, "s"),
        Some(LoadState::Ready(Arc::new(users("s"))))
    );
    assert!(catalog(&engine).from_cache);

    gate.add_permits(1);
    assert!(wait_until(TIMEOUT, || !catalog(&engine).from_cache));
    assert_eq!(
        schema(&engine, "s"),
        Some(LoadState::Ready(Arc::new(fresh)))
    );
    assert!(engine.catalog_revision() > revision);
}

/// Disconnects "c" and connects it again, waiting until it is `Connected`.
fn reconnect(engine: &Engine) {
    engine.disconnect("c");
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    wait_connected(engine);
}

#[test]
fn reconnect_reloads_catalog() {
    let fake = FakeDriver::new(script());
    let engine = connected(fake.clone());
    engine.load_catalog("c", CatalogNeed::Schemas);
    load_s(&engine);
    assert_eq!(fake.catalog_calls(), 1);
    let browses = fake.browse_calls();

    reconnect(&engine);
    assert!(
        wait_until(TIMEOUT, || fake.catalog_calls() == 2
            && fake.browse_calls() == browses + 1
            && !catalog(&engine).from_cache),
        "reconnect never reloaded the catalog: {} calls, {:?}",
        fake.catalog_calls(),
        engine.catalog("c")
    );
    assert!(schema_ready(&engine, "s"));
    assert!(matches!(
        catalog(&engine).schemas,
        Some(LoadState::Ready(_))
    ));
    assert_eq!(fake.catalog_calls(), 2);
}

#[test]
fn failed_schema_is_requested_once() {
    let fake = FakeDriver::new(script());
    let engine = connected(fake.clone());
    let missing = || CatalogNeed::Schema("missing".into());
    let failed = |engine: &Engine| {
        matches!(
            schema(engine, "missing"),
            Some(LoadState::Failed(e)) if e.message == "no such schema"
        )
    };
    engine.load_catalog("c", missing());
    assert!(wait_until(TIMEOUT, || failed(&engine)));
    assert_eq!(fake.catalog_calls(), 1);

    // As the editor does while its popup is open: the failed schema is asked for again.
    let revision = engine.catalog_revision();
    for _ in 0..5 {
        engine.load_catalog("c", missing());
        assert!(
            failed(&engine),
            "a failed schema does not go back to Loading"
        );
    }
    assert_eq!(engine.catalog_revision(), revision);
    load_s(&engine); // sync point
    assert_eq!(fake.catalog_calls(), 2, "missing once, s once");

    // The next session retries it, once.
    reconnect(&engine);
    assert!(wait_until(TIMEOUT, || fake.catalog_calls() == 4
        && failed(&engine)
        && !catalog(&engine).from_cache));
    engine.load_catalog("c", missing());
    load_s(&engine);
    assert_eq!(fake.catalog_calls(), 4);
}

#[test]
fn database_node_refresh_invalidates() {
    let database = |name: &str| Node {
        path: NodePath(vec![name.to_string()]),
        name: name.to_string(),
        kind: NodeKind::Database,
        has_children: true,
        detail: None,
    };
    let engine = connected(FakeDriver::new(FakeScript {
        tree: vec![database("main"), database("other")],
        schema_path: NodePath(vec!["main".into()]),
        ..script()
    }));
    load_s(&engine);
    let revision = engine.catalog_revision();

    // Deeper subtrees and other databases keep the catalog.
    engine.refresh("c", NodePath(vec!["main".into(), "s".into()]));
    engine.refresh("c", NodePath(vec!["other".into()]));
    assert!(schema_ready(&engine, "s"));
    assert_eq!(engine.catalog_revision(), revision);

    // The node holding the connection's schemas invalidates it.
    engine.refresh("c", NodePath(vec!["main".into()]));
    assert!(catalog(&engine).schema.is_empty());
    assert!(engine.catalog_revision() > revision);
}
