//! Applying grid edits (§7.7): changesets run in a transaction that refuses concurrent
//! changes.

mod support;

use std::sync::Arc;

use sqlr_core::{
    Change, Changeset, CommitMode, DriverError, ErrorKind, TableColumn, TableInfo, TableRef, Value,
};
use sqlr_engine::{
    ApplyReport, DriverRegistry, Engine, JobId, JobKind, JobOutcome, LoadState, QueryStatus,
    SessionKey, SessionStatus, Slot, TxPhase,
};
use sqlr_testkit::{FakeDriver, FakeScript};
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn engine_with(script: FakeScript) -> (Engine, Arc<FakeDriver>) {
    let fake = FakeDriver::new(script);
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Default::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
    (engine, fake)
}

fn tab() -> SessionKey {
    SessionKey {
        conn: "c".into(),
        slot: Slot::Tab(1),
    }
}

fn orders() -> TableRef {
    TableRef {
        database: None,
        schema: Some("s".into()),
        name: "orders".into(),
    }
}

fn update(id: i64, column: &str, value: Value) -> Change {
    Change::Update {
        key: vec![Value::Int(id)],
        column: column.into(),
        value,
    }
}

fn changeset(changes: Vec<Change>) -> Changeset {
    Changeset {
        table: orders(),
        key_columns: vec!["id".into()],
        changes,
    }
}

/// Opens the tab session with a first statement, so it is connected.
fn open_tab_session(engine: &Engine) {
    let q = engine.execute_in(&tab(), None, "select 1".into(), 10);
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(|r| r.read().status != QueryStatus::Running)));
}

fn outcome(engine: &Engine, job: JobId) -> JobOutcome {
    assert!(
        wait_until(TIMEOUT, || engine
            .job(job)
            .is_some_and(|j| j.outcome.is_some())),
        "the job never finished"
    );
    let state = engine.job(job).unwrap();
    assert_eq!(state.kind, JobKind::Apply);
    state.outcome.unwrap()
}

/// The statements run after the first `skip`, without the tab session's opening query.
fn executed_after(fake: &FakeDriver, skip: usize) -> Vec<String> {
    fake.executed()[skip..].to_vec()
}

#[test]
fn apply_runs_in_own_transaction() {
    let (engine, fake) = engine_with(FakeScript::default());
    open_tab_session(&engine);
    let before = fake.executed().len();
    let job = engine.apply_changeset(
        &tab(),
        changeset(vec![
            update(1, "total", Value::Int(5)),
            Change::Insert {
                values: vec![("total".into(), Value::Int(7))],
            },
            Change::Delete {
                key: vec![Value::Int(2)],
            },
        ]),
    );
    assert_eq!(
        outcome(&engine, job),
        JobOutcome::Applied(ApplyReport {
            applied: 3,
            failed: None
        })
    );
    assert_eq!(
        executed_after(&fake, before),
        [
            "BEGIN",
            "UPDATE s.orders SET total = ? WHERE id = ?",
            "INSERT INTO s.orders (total) VALUES (?)",
            "DELETE FROM s.orders WHERE id = ?",
            "COMMIT",
        ]
    );
    let with = fake.executed_with();
    assert_eq!(with[0].1, vec![Value::Int(5), Value::Int(1)]);
    assert_eq!(with[1].1, vec![Value::Int(7)]);
    assert_eq!(with[2].1, vec![Value::Int(2)]);
    // Auto-commit: the session's transaction state is idle again.
    assert_eq!(engine.transaction(&tab()).unwrap().phase, TxPhase::Idle);
}

#[test]
fn apply_refuses_concurrent_change() {
    let (engine, fake) = engine_with(FakeScript {
        dml_affected: vec![1, 0, 1],
        ..Default::default()
    });
    open_tab_session(&engine);
    let before = fake.executed().len();
    let job = engine.apply_changeset(
        &tab(),
        changeset(vec![
            update(1, "total", Value::Int(5)),
            update(2, "total", Value::Int(6)),
            update(3, "total", Value::Int(7)),
        ]),
    );
    let JobOutcome::Applied(report) = outcome(&engine, job) else {
        panic!("expected an apply report");
    };
    assert_eq!(report.applied, 0, "nothing stays applied");
    let (row, error) = report.failed.expect("the second row is named");
    assert_eq!(row, "row id=2");
    assert!(
        error.message.contains("changed or deleted"),
        "{}",
        error.message
    );
    let run = executed_after(&fake, before);
    assert_eq!(run.first().map(String::as_str), Some("BEGIN"));
    assert_eq!(run.last().map(String::as_str), Some("ROLLBACK"));
    assert!(!run.iter().any(|s| s == "COMMIT"), "{run:?}");
    // The third row was never tried.
    assert_eq!(run.len(), 4, "{run:?}");
}

#[test]
fn apply_joins_open_manual_transaction() {
    let (engine, fake) = engine_with(FakeScript::default());
    engine.set_commit_mode(&tab(), CommitMode::Manual);
    open_tab_session(&engine);
    assert_eq!(engine.transaction(&tab()).unwrap().phase, TxPhase::Open);
    let statements = engine.transaction(&tab()).unwrap().statements;
    let before = fake.executed().len();
    let job = engine.apply_changeset(&tab(), changeset(vec![update(1, "total", Value::Int(5))]));
    assert!(matches!(
        outcome(&engine, job),
        JobOutcome::Applied(ApplyReport {
            applied: 1,
            failed: None
        })
    ));
    let run = executed_after(&fake, before);
    assert!(
        !run.iter()
            .any(|s| s == "BEGIN" || s == "COMMIT" || s == "ROLLBACK"),
        "{run:?}"
    );
    assert!(run.iter().any(|s| s.starts_with("UPDATE")));
    // The changes are part of the open transaction, which the user commits.
    let tx = engine.transaction(&tab()).unwrap();
    assert_eq!(tx.phase, TxPhase::Open);
    assert_eq!(tx.statements, statements + 1);
}

#[test]
fn failed_apply_in_open_transaction_keeps_earlier_work() {
    let (engine, fake) = engine_with(FakeScript {
        dml_affected: vec![0],
        ..Default::default()
    });
    engine.set_commit_mode(&tab(), CommitMode::Manual);
    open_tab_session(&engine);
    let before = fake.executed().len();
    let job = engine.apply_changeset(&tab(), changeset(vec![update(1, "total", Value::Int(5))]));
    let JobOutcome::Applied(report) = outcome(&engine, job) else {
        panic!("expected an apply report");
    };
    assert_eq!(report.failed.unwrap().0, "row id=1");
    let run = executed_after(&fake, before);
    // Only the changeset is undone: the transaction stays open.
    assert!(!run.iter().any(|s| s == "ROLLBACK"), "{run:?}");
    assert!(
        run.iter().any(|s| s.starts_with("ROLLBACK TO SAVEPOINT")),
        "{run:?}"
    );
    assert_eq!(engine.transaction(&tab()).unwrap().phase, TxPhase::Open);
}

#[test]
fn manual_mode_apply_opens_the_session_transaction() {
    let (engine, fake) = engine_with(FakeScript::default());
    engine.set_commit_mode(&SessionKey::meta("c"), CommitMode::Manual);
    let before = fake.executed().len();
    let job = engine.apply_changeset(
        &SessionKey::meta("c"),
        changeset(vec![update(1, "total", Value::Int(5))]),
    );
    assert!(matches!(outcome(&engine, job), JobOutcome::Applied(r) if r.failed.is_none()));
    let run = executed_after(&fake, before);
    assert_eq!(run.first().map(String::as_str), Some("BEGIN"));
    assert!(!run.iter().any(|s| s == "COMMIT"), "{run:?}");
    assert_eq!(
        engine.transaction(&SessionKey::meta("c")).unwrap().phase,
        TxPhase::Open
    );
}

#[test]
fn apply_joins_open_manual_transaction_without_releasing_the_savepoint() {
    // SQL Server and Oracle have no RELEASE SAVEPOINT: the changeset must still apply and
    // keep the user's transaction open, and must not send a statement they reject.
    let (engine, fake) = engine_with(FakeScript {
        savepoints_never_released: true,
        ..Default::default()
    });
    engine.set_commit_mode(&tab(), CommitMode::Manual);
    open_tab_session(&engine);
    let before = fake.executed().len();
    let job = engine.apply_changeset(&tab(), changeset(vec![update(1, "total", Value::Int(5))]));
    assert!(
        matches!(
            outcome(&engine, job),
            JobOutcome::Applied(ApplyReport {
                applied: 1,
                failed: None
            })
        ),
        "the apply must not fail on a missing RELEASE SAVEPOINT"
    );
    let run = executed_after(&fake, before);
    assert!(run.iter().any(|s| s.starts_with("SAVEPOINT")), "{run:?}");
    assert!(
        !run.iter().any(|s| s.contains("RELEASE")),
        "no RELEASE SAVEPOINT on a server that has none: {run:?}"
    );
    assert_eq!(engine.transaction(&tab()).unwrap().phase, TxPhase::Open);
}

#[test]
fn failed_apply_without_release_undoes_only_the_changeset() {
    let (engine, fake) = engine_with(FakeScript {
        savepoints_never_released: true,
        dml_affected: vec![0],
        ..Default::default()
    });
    engine.set_commit_mode(&tab(), CommitMode::Manual);
    open_tab_session(&engine);
    let before = fake.executed().len();
    let job = engine.apply_changeset(&tab(), changeset(vec![update(1, "total", Value::Int(5))]));
    let JobOutcome::Applied(report) = outcome(&engine, job) else {
        panic!("expected an apply report");
    };
    assert_eq!(report.failed.unwrap().0, "row id=1");
    let run = executed_after(&fake, before);
    assert!(
        run.iter().any(|s| s.starts_with("ROLLBACK TO SAVEPOINT")),
        "{run:?}"
    );
    assert!(
        !run.iter().any(|s| s.contains("RELEASE")),
        "no RELEASE SAVEPOINT on a server that has none: {run:?}"
    );
    assert_eq!(engine.transaction(&tab()).unwrap().phase, TxPhase::Open);
}

#[test]
fn edit_key_column_uses_original_key() {
    let (engine, fake) = engine_with(FakeScript::default());
    open_tab_session(&engine);
    let job = engine.apply_changeset(&tab(), changeset(vec![update(1, "id", Value::Int(10))]));
    assert!(matches!(outcome(&engine, job), JobOutcome::Applied(r) if r.failed.is_none()));
    let with = fake.executed_with();
    assert_eq!(with[0].0, "UPDATE s.orders SET id = ? WHERE id = ?");
    assert_eq!(with[0].1, vec![Value::Int(10), Value::Int(1)]);
}

#[test]
fn apply_refused_on_a_read_only_session() {
    let (engine, fake) = engine_with(FakeScript::default());
    engine.set_read_only(&SessionKey::meta("c"), true);
    assert!(wait_until(TIMEOUT, || engine
        .transaction(&SessionKey::meta("c"))
        .is_some_and(|t| t.read_only)));
    let before = fake.executed().len();
    let job = engine.apply_changeset(
        &SessionKey::meta("c"),
        changeset(vec![update(1, "total", Value::Int(5))]),
    );
    let JobOutcome::Failed(e) = outcome(&engine, job) else {
        panic!("a read-only session refuses edits");
    };
    assert_eq!(e.kind, ErrorKind::Permission);
    assert!(executed_after(&fake, before).is_empty());
}

#[test]
fn apply_refused_when_the_table_changed_since_it_was_read() {
    let (engine, fake) = engine_with(FakeScript {
        origin_error: Some(DriverError::new(
            ErrorKind::Other,
            "table s.orders was renamed or dropped",
        )),
        ..Default::default()
    });
    open_tab_session(&engine);
    let before = fake.executed().len();
    let job = engine.apply_changeset(&tab(), changeset(vec![update(1, "total", Value::Int(5))]));
    let JobOutcome::Failed(e) = outcome(&engine, job) else {
        panic!("a stale origin refuses the changeset");
    };
    assert!(e.message.contains("renamed or dropped"), "{}", e.message);
    let run = executed_after(&fake, before);
    assert_eq!(run, ["BEGIN", "ROLLBACK"]);
}

#[test]
fn cancelled_apply_runs_nothing() {
    let (engine, fake) = engine_with(FakeScript {
        block_fetch_matching: Some("slow".into()),
        ..Default::default()
    });
    // A running query holds the session, so the changeset waits for its turn.
    let q = engine.execute_in(&tab(), None, "select slow".into(), 10);
    assert!(wait_until(TIMEOUT, || fake
        .executed()
        .iter()
        .any(|s| s == "select slow")));
    let before = fake.executed().len();
    let job = engine.apply_changeset(&tab(), changeset(vec![update(1, "total", Value::Int(5))]));
    engine.cancel_job(job);
    engine.cancel(q);
    assert_eq!(outcome(&engine, job), JobOutcome::Cancelled);
    let run = executed_after(&fake, before);
    assert!(!run.iter().any(|s| s.starts_with("UPDATE")), "{run:?}");
}

#[test]
fn describe_caches_table_info() {
    let info = TableInfo {
        table: orders(),
        columns: vec![TableColumn {
            name: "id".into(),
            type_name: "int8".into(),
            nullable: false,
            default: None,
            generated: false,
        }],
        primary_key: vec!["id".into()],
        unique_keys: vec![],
    };
    let (engine, _fake) = engine_with(FakeScript {
        tables: vec![info.clone()],
        ..Default::default()
    });
    assert_eq!(engine.table_info("c", &orders()), None);
    engine.describe(&SessionKey::meta("c"), orders());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.table_info("c", &orders()),
        Some(LoadState::Ready(_))
    )));
    assert_eq!(
        engine.table_info("c", &orders()),
        Some(LoadState::Ready(info))
    );
    let missing = TableRef {
        name: "nope".into(),
        ..orders()
    };
    engine.describe(&SessionKey::meta("c"), missing.clone());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.table_info("c", &missing),
        Some(LoadState::Failed(_))
    )));
}
