use sqlr_core::Secrets;
use sqlr_engine::{DriverRegistry, Engine, OperationKind, OperationState, SessionStatus};
use sqlr_testkit::{FakeDriver, FakeScript};
use std::{sync::Arc, time::Duration};
mod support;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};
#[test]
fn cancel_hung_connect_ends_cancelled_within_1s() {
    let fake = FakeDriver::new(FakeScript {
        block_on_connect: true,
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || engine.operations().iter().any(
        |o| o.kind == OperationKind::Connect && o.state == OperationState::Running
    )));
    let op = engine
        .operations()
        .into_iter()
        .find(|o| o.kind == OperationKind::Connect)
        .unwrap();
    assert!(engine.cancel_operation(op.id));
    assert!(wait_until(Duration::from_secs(1), || engine
        .operations()
        .iter()
        .any(|o| o.id == op.id && o.state == OperationState::Cancelled)));
    assert!(
        matches!(engine.session_status("c"),Some(SessionStatus::Failed(e)) if e.kind == sqlr_core::ErrorKind::Cancelled)
    );
    assert_eq!(fake.open_connections(), 0);
    assert!(!engine.cancel_operation(op.id));
    engine.forget_operation(op.id);
    assert!(engine.operations().iter().all(|o| o.id != op.id));
    // A retry owns an independent cancellation path.
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || engine.operations().iter().any(
        |o| o.kind == OperationKind::Connect && o.id != op.id && o.state == OperationState::Running
    )));
}
#[test]
fn finished_operations_expire_after_keep_finished() {
    let fake = FakeDriver::new(FakeScript::default());
    let mut drivers = DriverRegistry::default();
    drivers.register(fake);
    let engine = Engine::new(drivers, Arc::new(|| {}));
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
    assert!(wait_until(TIMEOUT, || engine
        .operations()
        .iter()
        .any(|o| o.state == OperationState::Done { ok: true })));
    engine.set_operations_retention(0);
    assert!(
        engine
            .operations()
            .iter()
            .all(|o| o.state == OperationState::Running)
    );
}

#[test]
fn query_and_script_cancel_follow_existing_stop_paths() {
    use sqlr_engine::{QueryStatus, ScriptOnError};
    let fake = FakeDriver::new(FakeScript {
        block_on_fetch: true,
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
    let q = engine.execute("c", "select 1".into(), 10);
    assert!(wait_until(TIMEOUT, || engine.operations().iter().any(
        |o| o.kind == OperationKind::Query && o.state == OperationState::Running
    )));
    let op = engine
        .operations()
        .into_iter()
        .find(|o| o.kind == OperationKind::Query && o.state == OperationState::Running)
        .unwrap();
    assert!(engine.cancel_operation(op.id));
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .unwrap()
        .read()
        .status
        == QueryStatus::Cancelled));
    assert!(wait_until(TIMEOUT, || engine
        .operations()
        .iter()
        .any(|o| o.id == op.id && o.state == OperationState::Cancelled)));
    let script = engine.execute_script(
        "c",
        vec!["select 2".into(), "select 3".into()],
        10,
        ScriptOnError::Stop,
    );
    assert!(wait_until(TIMEOUT, || engine.operations().iter().any(
        |o| o.kind == OperationKind::Script && o.state == OperationState::Running
    )));
    let op = engine
        .operations()
        .into_iter()
        .find(|o| o.kind == OperationKind::Script && o.state == OperationState::Running)
        .unwrap();
    assert!(engine.cancel_operation(op.id));
    assert!(wait_until(TIMEOUT, || engine
        .script(script)
        .unwrap()
        .read()
        .done));
    assert!(wait_until(TIMEOUT, || engine
        .operations()
        .iter()
        .any(|o| o.id == op.id && o.state == OperationState::Cancelled)));
    assert!(!fake.executed().iter().any(|s| s == "select 3"));
}

#[test]
fn disconnect_cancels_pending_connect_without_waiting_for_timeout() {
    let fake = FakeDriver::new(FakeScript {
        block_on_connect: true,
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake);
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || engine
        .operations()
        .iter()
        .any(|o| o.kind == OperationKind::Connect)));
    engine.disconnect("c");
    assert!(wait_until(Duration::from_millis(500), || engine
        .operations()
        .iter()
        .all(|o| o.state != OperationState::Running)));
}

#[test]
fn catalog_read_is_cancelable_and_leaves_loading_state() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let fake = FakeDriver::new(FakeScript {
        catalog_gate: Some(gate),
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake);
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
    engine.load_catalog("c", sqlr_core::CatalogNeed::Schema("blocked".into()));
    assert!(wait_until(TIMEOUT, || engine.operations().iter().any(
        |o| o.kind == OperationKind::Browse && o.state == OperationState::Running
    )));
    let op = engine
        .operations()
        .into_iter()
        .find(|o| o.kind == OperationKind::Browse && o.state == OperationState::Running)
        .unwrap();
    assert!(engine.cancel_operation(op.id));
    assert!(wait_until(TIMEOUT, || engine
        .operations()
        .iter()
        .any(|o| o.id == op.id && o.state == OperationState::Cancelled)));
    assert!(
        matches!(engine.catalog("c").unwrap().schema.get("blocked"),Some(sqlr_engine::LoadState::Failed(e)) if e.kind==sqlr_core::ErrorKind::Cancelled)
    );
}
