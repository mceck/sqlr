use std::path::PathBuf;

use sqlr_core::{
    ColumnMapping, DumpOptions, Endpoint, SavedTask, TableRef, TaskSpec, TransferOptions,
    TransferSpec,
};
use sqlr_store::Db;

fn transfer_task(id: &str, name: &str) -> SavedTask {
    SavedTask {
        id: id.into(),
        name: name.into(),
        spec: TaskSpec::Transfer(TransferSpec {
            source: Endpoint::Table {
                conn: "pg-1".into(),
                table: TableRef {
                    database: None,
                    schema: Some("public".into()),
                    name: "users".into(),
                },
            },
            target: Endpoint::NewTable {
                conn: "lite-1".into(),
                table: TableRef {
                    database: None,
                    schema: Some("main".into()),
                    name: "users".into(),
                },
                ddl: None,
            },
            mapping: vec![ColumnMapping::new(0, Some("id".into()))],
            options: TransferOptions::default(),
        }),
        created_ms: 10,
        updated_ms: 10,
    }
}

#[test]
fn tasks_and_runs_round_trip_and_retention() {
    let db = Db::open_in_memory().unwrap();
    db.save_task(&transfer_task("t1", "Copy users")).unwrap();
    db.save_task(&SavedTask {
        id: "t2".into(),
        name: "Nightly dump".into(),
        spec: TaskSpec::Dump {
            conn: "pg-1".into(),
            database: "app".into(),
            options: DumpOptions::default(),
            out: PathBuf::from("/tmp/app.sql"),
        },
        created_ms: 11,
        updated_ms: 11,
    })
    .unwrap();

    // Tasks come back whole, sorted by name.
    let tasks = db.list_tasks().unwrap();
    let names: Vec<_> = tasks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Copy users", "Nightly dump"]);
    assert_eq!(tasks[0], transfer_task("t1", "Copy users"));

    // Saving again under the same id replaces it.
    db.save_task(&transfer_task("t1", "Copy users v2")).unwrap();
    assert_eq!(db.list_tasks().unwrap().len(), 2);
    assert!(
        db.list_tasks()
            .unwrap()
            .iter()
            .any(|t| t.name == "Copy users v2")
    );

    // 51 runs: the newest 50 stay, newest first.
    for i in 0..51 {
        let run = db.add_run("t1", 1000 + i).unwrap();
        db.finish_run(run, 2000 + i, "ok", &format!("run {i}"), "log")
            .unwrap();
    }
    let runs = db.runs("t1", 100).unwrap();
    assert_eq!(runs.len(), 50);
    assert_eq!(runs[0].summary, "run 50");
    assert_eq!(runs[49].summary, "run 1", "the oldest run was dropped");
    assert_eq!(runs[0].ended_ms, Some(2050));
    assert_eq!(db.runs("t1", 3).unwrap().len(), 3);
    assert!(
        db.runs("t2", 10).unwrap().is_empty(),
        "runs belong to their task"
    );

    // A run that never finished has no end.
    let open = db.add_run("t2", 5).unwrap();
    let got = db.runs("t2", 10).unwrap();
    assert_eq!((got[0].id, got[0].ended_ms), (open, None));

    // Logs are capped at 1 MiB, keeping their end.
    let big = format!("start{}end", "x".repeat(2 << 20));
    db.finish_run(open, 6, "ok", "big", &big).unwrap();
    let log = &db.runs("t2", 1).unwrap()[0].log;
    assert!(log.len() <= (1 << 20) + 16, "{}", log.len());
    assert!(log.ends_with("end"));

    // Deleting a task deletes its runs.
    db.delete_task("t1").unwrap();
    assert_eq!(db.list_tasks().unwrap().len(), 1);
    assert!(db.runs("t1", 100).unwrap().is_empty());
}

#[test]
fn task_json_has_no_secrets() {
    // A task names connections by id; whatever secret those connections have is not in the
    // spec, so it cannot reach the database.
    let db = Db::open_in_memory().unwrap();
    db.save_task(&transfer_task("t1", "Copy users")).unwrap();
    let stored: String = db.task_spec_json("t1").unwrap().unwrap();
    assert!(stored.contains("pg-1"));
    for word in ["password", "secret", "token"] {
        assert!(!stored.to_lowercase().contains(word), "{word}: {stored}");
    }
}
