//! Object definitions and DDL applies: loading, the statement placement rule,
//! transactional and per-statement applies, refusals, and what an apply refreshes.

use std::sync::Arc;

use sqlr_core::{
    CatalogColumn, CatalogNeed, CatalogRelation, ColumnDef, CommitMode, ConnectionProfile,
    ErrorKind, Node, NodeKind, NodePath, ObjectDef, ObjectEditing, ObjectKind, RelationKind,
    SchemaCatalog, SchemaChange, Secrets, TableDef, TableRef,
};
use sqlr_engine::{
    ApplyOptions, DdlStepOutcome, DriverRegistry, Engine, JobId, JobKind, JobOutcome, LoadState,
    SessionKey, SessionStatus,
};
use sqlr_testkit::{FakeDriver, FakeScript};

mod support;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn table(name: &str, columns: &[&str]) -> TableDef {
    let mut def = TableDef::new(TableRef {
        database: None,
        schema: None,
        name: name.into(),
    });
    def.columns = columns
        .iter()
        .map(|c| ColumnDef {
            name: c.to_string(),
            type_name: "integer".into(),
            nullable: true,
            ..Default::default()
        })
        .collect();
    def
}

fn node(parent: &NodePath, name: &str, kind: NodeKind) -> Node {
    Node {
        path: parent.child(name),
        name: name.into(),
        kind,
        has_children: true,
        detail: None,
    }
}

/// `s / tables / orders`, with `orders` loadable and creatable tables under `tables`.
fn editing_script(script: FakeScript) -> FakeScript {
    let root = NodePath::root();
    let s = node(&root, "s", NodeKind::Schema);
    let tables = node(&s.path, "tables", NodeKind::Folder);
    let orders = node(&tables.path, "orders", NodeKind::Table);
    FakeScript {
        editing: Some(ObjectEditing {
            create: vec![ObjectKind::Table],
            alter: vec![ObjectKind::Table],
            drop: vec![ObjectKind::Table],
            rename: vec![ObjectKind::Table],
            transactional_ddl: true,
            ..Default::default()
        }),
        objects: vec![ObjectDef::Table(table("orders", &["id", "total"]))],
        tables_folder: Some(tables.path.clone()),
        tree: vec![s, tables, orders],
        catalog: vec![SchemaCatalog {
            schema: "s".into(),
            relations: vec![CatalogRelation {
                name: "orders".into(),
                kind: RelationKind::Table,
                columns: vec![CatalogColumn {
                    name: "id".into(),
                    type_name: "integer".into(),
                }],
            }],
        }],
        ..script
    }
}

fn setup(script: FakeScript, profile: ConnectionProfile) -> (Engine, Arc<FakeDriver>) {
    let fake = FakeDriver::new(editing_script(script));
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), profile);
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
    (engine, fake)
}

/// The job's outcome once it ended; object loads and DDL applies are general jobs.
fn finished(engine: &Engine, id: JobId) -> JobOutcome {
    assert!(
        wait_until(TIMEOUT, || engine
            .job(id)
            .is_some_and(|j| j.outcome.is_some())),
        "job {id:?} still running"
    );
    engine.job(id).unwrap().outcome.unwrap()
}

fn apply(engine: &Engine, statements: &[&str], transactional: bool) -> JobOutcome {
    let id = engine.apply_ddl(
        &SessionKey::meta("c"),
        statements.iter().map(|s| s.to_string()).collect(),
        ApplyOptions {
            transactional,
            ..Default::default()
        },
    );
    finished(engine, id)
}

fn failing() -> FakeScript {
    FakeScript {
        fail_matching: Some("boom".into()),
        ..Default::default()
    }
}

#[test]
fn load_object_returns_the_definition() {
    let (engine, _) = setup(FakeScript::default(), dev());
    let orders = Node {
        path: NodePath::root().child("s").child("tables").child("orders"),
        name: "orders".into(),
        kind: NodeKind::Table,
        has_children: true,
        detail: None,
    };
    let object = engine
        .object_ref("c", &orders)
        .expect("orders is an object");
    let id = engine.load_object("c", object);
    match finished(&engine, id) {
        JobOutcome::Object(ObjectDef::Table(def)) => assert_eq!(def.columns.len(), 2),
        other => panic!("{other:?}"),
    }
    let missing = engine.load_object(
        "c",
        sqlr_core::ObjectRef::relation(ObjectKind::Table, &table("nope", &[]).table),
    );
    assert!(matches!(finished(&engine, missing), JobOutcome::Failed(_)));
    assert_eq!(engine.job(id).unwrap().kind, JobKind::LoadObject);
    engine.forget_job(id);
    assert!(engine.job(id).is_none());
}

#[test]
fn load_ddl_writes_the_create_statement() {
    let (engine, _) = setup(FakeScript::default(), dev());
    let tables = NodePath::root().child("s").child("tables");
    let orders = node(&tables, "orders", NodeKind::Table);
    let id = engine.load_ddl(&SessionKey::meta("c"), orders);
    match finished(&engine, id) {
        JobOutcome::Ddl(text) => {
            assert!(text.contains("orders"), "{text}");
            assert!(text.contains("total"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(engine.job(id).unwrap().kind, JobKind::LoadDdl);
    // A folder is no object: no DDL.
    let folder = node(&NodePath::root().child("s"), "tables", NodeKind::Folder);
    let id = engine.load_ddl(&SessionKey::meta("c"), folder);
    assert!(matches!(finished(&engine, id), JobOutcome::Failed(_)));
    // Nor on a connection that is not connected.
    let id = engine.load_ddl(
        &SessionKey::meta("nope"),
        node(&tables, "orders", NodeKind::Table),
    );
    assert!(matches!(finished(&engine, id), JobOutcome::Failed(_)));
}

#[test]
fn apply_transactional_rolls_back_on_failure() {
    let (engine, fake) = setup(failing(), dev());
    let JobOutcome::DdlApplied(report) =
        apply(&engine, &["CREATE a", "ALTER boom", "CREATE c"], true)
    else {
        panic!("not applied")
    };
    assert_eq!(
        fake.executed(),
        ["BEGIN", "CREATE a", "ALTER boom", "ROLLBACK"]
    );
    let outcomes: Vec<&DdlStepOutcome> = report.statements.iter().map(|(_, o)| o).collect();
    assert!(matches!(
        outcomes.as_slice(),
        [
            DdlStepOutcome::RolledBack,
            DdlStepOutcome::Failed(_),
            DdlStepOutcome::Skipped
        ]
    ));
    assert!(report.rolled_back);
    assert!(!report.ok());
    assert_eq!(
        report.failure().unwrap().message,
        "syntax error near \"boom\""
    );

    // Alone statements run outside the transaction; cleanup runs even after a failure.
    let before = fake.executed().len();
    let JobOutcome::DdlApplied(report) = apply(
        &engine,
        &[
            "PRAGMA foreign_keys = OFF",
            "CREATE a",
            "ALTER boom",
            "PRAGMA foreign_keys = ON",
        ],
        true,
    ) else {
        panic!("not applied")
    };
    assert_eq!(
        fake.executed()[before..],
        [
            "PRAGMA foreign_keys = OFF",
            "BEGIN",
            "CREATE a",
            "ALTER boom",
            "ROLLBACK",
            "PRAGMA foreign_keys = ON"
        ]
    );
    let outcomes: Vec<&DdlStepOutcome> = report.statements.iter().map(|(_, o)| o).collect();
    assert!(matches!(
        outcomes.as_slice(),
        [
            DdlStepOutcome::Applied,
            DdlStepOutcome::RolledBack,
            DdlStepOutcome::Failed(_),
            DdlStepOutcome::Applied
        ]
    ));

    // Everything applied: one transaction, committed.
    let before = fake.executed().len();
    let JobOutcome::DdlApplied(report) = apply(&engine, &["CREATE a", "CREATE b"], true) else {
        panic!("not applied")
    };
    assert!(report.ok() && !report.rolled_back);
    assert_eq!(
        fake.executed()[before..],
        ["BEGIN", "CREATE a", "CREATE b", "COMMIT"]
    );
}

#[test]
fn apply_per_statement_reports_skipped() {
    let (engine, fake) = setup(failing(), dev());
    let JobOutcome::DdlApplied(report) =
        apply(&engine, &["CREATE a", "ALTER boom", "CREATE c"], false)
    else {
        panic!("not applied")
    };
    assert_eq!(fake.executed(), ["CREATE a", "ALTER boom"]);
    let outcomes: Vec<&DdlStepOutcome> = report.statements.iter().map(|(_, o)| o).collect();
    assert!(matches!(
        outcomes.as_slice(),
        [
            DdlStepOutcome::Applied,
            DdlStepOutcome::Failed(_),
            DdlStepOutcome::Skipped
        ]
    ));
    assert!(!report.rolled_back);
    assert_eq!(
        report.summary(),
        "Statement 2 of 3 failed: syntax error near \"boom\". \
         The DDL is not transactional: 1 statement before the failure was applied."
    );
}

#[test]
fn apply_refreshes_explorer_and_catalog() {
    let (engine, fake) = setup(FakeScript::default(), dev());
    let tables = NodePath::root().child("s").child("tables");
    engine.browse("c", NodePath::root());
    engine.browse("c", NodePath::root().child("s"));
    engine.browse("c", tables.clone());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.children("c", &tables),
        Some(LoadState::Ready(_))
    )));
    engine.load_catalog("c", CatalogNeed::Schema("s".into()));
    assert!(wait_until(TIMEOUT, || engine.catalog("c").is_some_and(
        |c| matches!(c.schema.get("s"), Some(LoadState::Ready(_)))
    )));

    let statements = engine
        .preview_ddl(
            "c",
            &[SchemaChange::Create(ObjectDef::Table(table(
                "customers",
                &["id"],
            )))],
        )
        .unwrap();
    assert_eq!(statements, ["CREATE TABLE customers (id integer)"]);
    let id = engine.apply_ddl(
        &SessionKey::meta("c"),
        statements,
        ApplyOptions {
            transactional: true,
            database: None,
            refresh: vec![tables.clone()],
            masking: None,
        },
    );
    let JobOutcome::DdlApplied(report) = finished(&engine, id) else {
        panic!("not applied")
    };
    assert!(report.ok());
    // The catalog is forgotten and the refreshed folder lists the new table.
    assert!(engine.catalog("c").is_none());
    assert!(
        wait_until(TIMEOUT, || match engine.children("c", &tables) {
            Some(LoadState::Ready(nodes)) => nodes.iter().any(|n| n.name == "customers"),
            _ => false,
        }),
        "{:?}",
        fake.tree()
    );
    // And the definition loads.
    let id = engine.load_object(
        "c",
        sqlr_core::ObjectRef::relation(ObjectKind::Table, &table("customers", &[]).table),
    );
    assert!(matches!(finished(&engine, id), JobOutcome::Object(_)));
}

#[test]
fn apply_refused_on_read_only() {
    let read_only = ConnectionProfile {
        read_only: true,
        ..dev()
    };
    let (engine, fake) = setup(FakeScript::default(), read_only);
    match apply(&engine, &["CREATE a"], true) {
        JobOutcome::Failed(e) => assert_eq!(e.kind, ErrorKind::Permission),
        other => panic!("{other:?}"),
    }
    assert!(fake.executed().is_empty(), "{:?}", fake.executed());
}

#[test]
fn apply_refused_inside_an_open_transaction() {
    let manual = ConnectionProfile {
        commit_mode: CommitMode::Manual,
        ..dev()
    };
    let (engine, fake) = setup(FakeScript::default(), manual);
    let key = SessionKey::meta("c");
    let q = engine.execute_in(&key, None, "select 1".into(), 10);
    assert!(wait_until(TIMEOUT, || engine
        .transaction(&key)
        .is_some_and(|t| t.phase != sqlr_engine::TxPhase::Idle)));
    engine.close_result(q);
    let before = fake.executed().len();
    match apply(&engine, &["CREATE a"], true) {
        JobOutcome::Failed(e) => assert!(e.message.contains("transaction"), "{e:?}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.executed().len(), before);
}

#[test]
fn sync_ddl_needs_a_connected_dialect() {
    let (engine, _) = setup(FakeScript::default(), dev());
    let before = table("orders", &["id"]);
    let after = table("orders", &["id", "total"]);
    assert_eq!(
        engine.table_ddl("c", &before, &after).unwrap(),
        ["ALTER TABLE orders ADD COLUMN total integer"]
    );
    assert_eq!(
        engine
            .create_ddl("c", &ObjectDef::Table(before.clone()))
            .unwrap(),
        "CREATE TABLE orders (id integer);"
    );
    let err = engine.table_ddl("other", &before, &after).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Connection);
}

#[test]
fn apply_refused_while_any_session_has_a_transaction() {
    let manual = ConnectionProfile {
        commit_mode: CommitMode::Manual,
        ..dev()
    };
    let (engine, fake) = setup(FakeScript::default(), manual);
    // Sessions per tab are on (the default): tab 1 opens its own transaction and writes.
    let tab = engine.session("c", Some(1));
    assert_ne!(tab, SessionKey::meta("c"));
    let q = engine.execute_in(&tab, None, "insert into t values (1)".into(), 10);
    assert!(wait_until(TIMEOUT, || engine
        .transaction(&tab)
        .is_some_and(|t| t.wrote)));
    engine.close_result(q);
    let before = fake.executed().len();
    match apply(&engine, &["CREATE a"], true) {
        JobOutcome::Failed(e) => assert_eq!(
            e.message,
            "A transaction is open in tab 1: commit or roll back first"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.executed().len(), before);
}

#[test]
fn a_transaction_that_only_read_elsewhere_does_not_block_applies() {
    let manual = ConnectionProfile {
        commit_mode: CommitMode::Manual,
        ..dev()
    };
    let (engine, _) = setup(FakeScript::default(), manual);
    // An editor tab ran its SELECT in Manual mode: an idle transaction with no writes.
    let tab = engine.session("c", Some(1));
    let q = engine.execute_in(&tab, None, "select 1".into(), 10);
    assert!(wait_until(TIMEOUT, || engine
        .transaction(&tab)
        .is_some_and(|t| t.phase != sqlr_engine::TxPhase::Idle)));
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(
            |r| r.read().status != sqlr_engine::QueryStatus::Running
        )));
    engine.close_result(q);
    match apply(&engine, &["CREATE a"], true) {
        JobOutcome::DdlApplied(report) => assert!(report.ok(), "{report:?}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn begin_and_commit_failures_are_their_own_steps() {
    let (engine, fake) = setup(
        FakeScript {
            fail_matching: Some("BEGIN".into()),
            ..Default::default()
        },
        dev(),
    );
    let JobOutcome::DdlApplied(report) = apply(&engine, &["CREATE a", "CREATE b"], true) else {
        panic!("not applied")
    };
    assert_eq!(fake.executed(), ["BEGIN"]);
    assert!(matches!(
        report.statements.iter().map(|(_, o)| o).collect::<Vec<_>>()[..],
        [DdlStepOutcome::Skipped, DdlStepOutcome::Skipped]
    ));
    let (verb, e) = report.control.first().expect("the BEGIN failure");
    assert_eq!(verb, "BEGIN");
    assert_eq!(report.failure(), Some(e));
    assert!(
        report.summary().starts_with("BEGIN failed:"),
        "{}",
        report.summary()
    );

    let (engine, _) = setup(
        FakeScript {
            fail_matching: Some("COMMIT".into()),
            ..Default::default()
        },
        dev(),
    );
    let JobOutcome::DdlApplied(report) = apply(&engine, &["CREATE a", "CREATE b"], true) else {
        panic!("not applied")
    };
    assert!(matches!(
        report.statements.iter().map(|(_, o)| o).collect::<Vec<_>>()[..],
        [DdlStepOutcome::RolledBack, DdlStepOutcome::RolledBack]
    ));
    assert_eq!(report.control[0].0, "COMMIT");
    assert!(report.rolled_back);
    assert!(!report.ok());
}

#[test]
fn cleanup_runs_after_a_failure_without_transactions() {
    let (engine, fake) = setup(failing(), dev());
    let JobOutcome::DdlApplied(report) = apply(
        &engine,
        &[
            "PRAGMA foreign_keys = OFF",
            "ALTER boom",
            "CREATE c",
            "PRAGMA foreign_keys = ON",
        ],
        false,
    ) else {
        panic!("not applied")
    };
    assert_eq!(
        fake.executed(),
        [
            "PRAGMA foreign_keys = OFF",
            "ALTER boom",
            "PRAGMA foreign_keys = ON"
        ]
    );
    assert!(matches!(
        report.statements.iter().map(|(_, o)| o).collect::<Vec<_>>()[..],
        [
            DdlStepOutcome::Applied,
            DdlStepOutcome::Failed(_),
            DdlStepOutcome::Skipped,
            DdlStepOutcome::Applied
        ]
    ));
}
