//! Creating, altering and dropping a table through the engine, against
//! a real driver. Each driver suite includes this file with `#[path]`; it is no test target
//! of the engine itself.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlr_core::{
    ColumnDef, ConnectionConfig, ConnectionProfile, ConstraintDef, Driver, ObjectDef, ObjectKind,
    ObjectRef, SchemaChange, Secrets, TableDef, TableRef,
};
use sqlr_engine::{
    ApplyOptions, DriverRegistry, Engine, JobId, JobOutcome, SessionKey, SessionStatus,
};

fn wait(engine: &Engine, what: &str, mut done: impl FnMut(&Engine) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(engine) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn job(engine: &Engine, id: JobId) -> JobOutcome {
    wait(engine, "the job", |e| {
        e.job(id).is_some_and(|j| j.outcome.is_some())
    });
    engine.job(id).unwrap().outcome.unwrap()
}

fn apply(engine: &Engine, conn: &str, statements: Vec<String>, transactional: bool) {
    let id = engine.apply_ddl(
        &SessionKey::meta(conn),
        statements.clone(),
        ApplyOptions {
            transactional,
            ..Default::default()
        },
    );
    match job(engine, id) {
        JobOutcome::DdlApplied(report) => assert!(report.ok(), "{statements:#?}: {report:#?}"),
        other => panic!("{statements:#?}: {other:?}"),
    }
}

fn load(engine: &Engine, conn: &str, table: &TableRef) -> JobOutcome {
    let id = engine.load_object(conn, ObjectRef::relation(ObjectKind::Table, table));
    job(engine, id)
}

fn column(name: &str, type_name: &str, nullable: bool) -> ColumnDef {
    ColumnDef {
        name: name.into(),
        type_name: type_name.into(),
        nullable,
        ..Default::default()
    }
}

/// Connects `driver` through an engine, creates `schema.e2e` from a [`SchemaChange::Create`],
/// renames a column and adds one, then drops the table.
pub fn create_alter_drop(
    driver: Arc<dyn Driver>,
    cfg: ConnectionConfig,
    secrets: Secrets,
    schema: &str,
    int_type: &str,
) {
    let transactional = driver
        .manifest()
        .editing
        .as_ref()
        .expect("object editing")
        .transactional_ddl;
    let conn = cfg.id.clone();
    let mut drivers = DriverRegistry::default();
    drivers.register(driver);
    let engine = Engine::new(drivers, Arc::new(|| {}));
    engine.connect(cfg, secrets, ConnectionProfile::development());
    wait(&engine, "the connection", |e| {
        matches!(e.session_status(&conn), Some(SessionStatus::Connected(_)))
    });

    let table = TableRef {
        database: None,
        schema: Some(schema.into()),
        name: "e2e".into(),
    };
    let mut def = TableDef::new(table.clone());
    def.columns = vec![
        column("id", int_type, false),
        column("name", "varchar(20)", true),
    ];
    def.constraints.push(ConstraintDef::PrimaryKey {
        name: None,
        columns: vec!["id".into()],
    });
    let create = engine
        .preview_ddl(&conn, &[SchemaChange::Create(ObjectDef::Table(def))])
        .unwrap();
    apply(&engine, &conn, create, transactional);
    let JobOutcome::Object(ObjectDef::Table(loaded)) = load(&engine, &conn, &table) else {
        panic!("e2e did not load")
    };
    assert_eq!(loaded.columns.len(), 2);

    let mut after = loaded.clone();
    after.columns[1].name = "label".into();
    after.columns.push(column("extra", int_type, true));
    let alter = engine.table_ddl(&conn, &loaded, &after).unwrap();
    apply(&engine, &conn, alter, transactional);
    let JobOutcome::Object(ObjectDef::Table(altered)) = load(&engine, &conn, &table) else {
        panic!("e2e did not load")
    };
    let names: Vec<&str> = altered.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "label", "extra"]);

    let drop = engine
        .preview_ddl(
            &conn,
            &[SchemaChange::Drop {
                object: ObjectRef::relation(ObjectKind::Table, &table),
                cascade: false,
            }],
        )
        .unwrap();
    apply(&engine, &conn, drop, transactional);
    assert!(matches!(
        load(&engine, &conn, &table),
        JobOutcome::Failed(_)
    ));
}
