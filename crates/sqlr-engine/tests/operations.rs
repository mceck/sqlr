use crate::support;
use sqlr_core::Secrets;
use sqlr_engine::{DriverRegistry, Engine, OperationKind, OperationState, SessionStatus};
use sqlr_testkit::{FakeDriver, FakeScript};
use std::{sync::Arc, time::Duration};
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
fn an_engine_task_panic_fails_that_query_and_the_engine_keeps_running() {
    use sqlr_engine::QueryStatus;
    let fake = FakeDriver::new(FakeScript {
        panic_matching: Some("boom".into()),
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

    let q = engine.execute("c", "select boom".into(), 10);
    assert!(wait_until(TIMEOUT, || matches!(
        &engine.result(q).unwrap().read().status,
        QueryStatus::Failed(e) if e.message.contains("internal panic")
    )));
    assert!(wait_until(TIMEOUT, || {
        engine.operations().iter().any(
        |o| o.kind == OperationKind::Query
            && matches!(&o.state, OperationState::Failed(message) if message.contains("internal panic"))
    )
    }));

    // The session and the runtime survived: another statement still runs.
    let q = engine.execute("c", "select 1".into(), 10);
    assert!(wait_until(TIMEOUT, || matches!(
        engine.result(q).unwrap().read().status,
        QueryStatus::Ready { .. }
    )));
}

#[test]
fn a_connect_panic_wakes_queries_waiting_for_the_connection() {
    let mut drivers = DriverRegistry::default();
    drivers.register(FakeDriver::new(FakeScript {
        panic_on_connect: true,
        ..Default::default()
    }));
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    let query = engine.execute("c", "select 1".into(), 10);
    assert!(
        wait_until(TIMEOUT, || matches!(
            engine.result(query).unwrap().read().status,
            sqlr_engine::QueryStatus::Failed(_)
        )),
        "a waiting query must settle after connect panics"
    );
}

#[test]
fn a_panic_in_a_background_job_settles_the_job_and_operation() {
    let mut drivers = DriverRegistry::default();
    drivers.register(FakeDriver::new(FakeScript {
        panic_matching: Some("boom".into()),
        ..Default::default()
    }));
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
    let job = engine.apply_ddl(
        &sqlr_engine::SessionKey::meta("c"),
        vec!["boom".into()],
        Default::default(),
    );
    assert!(
        wait_until(TIMEOUT, || matches!(
            engine.job(job).unwrap().outcome,
            Some(sqlr_engine::JobOutcome::Failed(_))
        )),
        "a panicking DDL job must settle"
    );
    assert!(engine.operations().iter().any(|o| matches!(&o.state, OperationState::Failed(message) if message.contains("internal panic"))));
}

#[test]
fn a_failed_query_logs_one_structured_line_subject_to_the_sql_setting() {
    use sqlr_core::ErrorKind;
    use std::sync::{Mutex, OnceLock};
    use tracing::Subscriber;
    use tracing::field::{Field, Visit};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry::LookupSpan;

    #[derive(Clone, Debug, Default)]
    struct Event {
        level: String,
        fields: std::collections::BTreeMap<String, String>,
    }
    #[derive(Default)]
    struct Collector(std::collections::BTreeMap<String, String>);
    impl Visit for Collector {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().into(), format!("{value:?}"));
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.insert(field.name().into(), value.into());
        }
    }
    struct Capture;
    impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = Collector::default();
            event.record(&mut visitor);
            events().lock().unwrap().push(Event {
                level: event.metadata().level().to_string(),
                fields: visitor.0,
            });
        }
    }
    fn events() -> &'static Mutex<Vec<Event>> {
        static EVENTS: OnceLock<Mutex<Vec<Event>>> = OnceLock::new();
        EVENTS.get_or_init(Default::default)
    }
    fn install() {
        static ONCE: OnceLock<()> = OnceLock::new();
        ONCE.get_or_init(|| {
            let subscriber = tracing_subscriber::registry().with(Capture);
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }

    install();
    let fake = FakeDriver::new(FakeScript {
        fail_matching: Some("bad".into()),
        rows: 20,
        fail_after_chunks: Some((
            1,
            sqlr_core::DriverError::new(ErrorKind::Syntax, "fetch failed"),
        )),
        ..Default::default()
    });
    let mut drivers = DriverRegistry::default();
    drivers.register(fake);
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("logging-c", "fake"), Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("logging-c"),
        Some(SessionStatus::Connected(_))
    )));

    let query = |sql: &str| {
        let q = engine.execute("logging-c", sql.into(), 10);
        assert!(wait_until(TIMEOUT, || matches!(
            &engine.result(q).unwrap().read().status,
            sqlr_engine::QueryStatus::Failed(e) if e.kind == ErrorKind::Syntax
        )));
    };
    query("select bad");
    let lines: Vec<Event> = events()
        .lock()
        .unwrap()
        .iter()
        .filter(|e| {
            e.fields.get("op").map(String::as_str) == Some("query")
                && e.fields.get("conn").map(String::as_str) == Some("logging-c")
                && e.fields.get("error_kind").map(String::as_str) == Some("Syntax")
        })
        .cloned()
        .collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(
        line.level, "INFO",
        "an expected syntax error is not a warning"
    );
    assert_eq!(line.fields.get("driver").map(String::as_str), Some("fake"));
    assert_eq!(
        line.fields.get("sql").map(String::as_str),
        Some("select bad")
    );
    assert!(line.fields.contains_key("elapsed_ms"));
    assert!(!line.fields.contains_key("incident"));

    engine.set_include_sql(false);
    query("select bad again");
    let last = events()
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e.fields.get("op").map(String::as_str) == Some("query"))
        .cloned()
        .unwrap();
    assert!(!last.fields.contains_key("sql"));
    assert_eq!(
        last.fields.get("message").map(String::as_str),
        Some("query failed")
    );
    let q = engine.execute("logging-c", "select 1".into(), 10);
    assert!(wait_until(TIMEOUT, || matches!(
        engine.result(q).unwrap().read().status,
        sqlr_engine::QueryStatus::Ready { more: true }
    )));
    engine.fetch_more(q);
    assert!(
        wait_until(TIMEOUT, || events().lock().unwrap().iter().any(|e| e
            .fields
            .get("op")
            .map(String::as_str)
            == Some("fetch"))),
        "a later fetch failure must be logged"
    );
    let job = engine.apply_ddl(
        &sqlr_engine::SessionKey::meta("logging-c"),
        vec!["bad DDL".into()],
        Default::default(),
    );
    assert!(wait_until(TIMEOUT, || engine
        .job(job)
        .unwrap()
        .outcome
        .is_some()));
    assert!(
        wait_until(TIMEOUT, || events().lock().unwrap().iter().any(|e| e
            .fields
            .get("op")
            .map(String::as_str)
            == Some("job"))),
        "DDL reports containing failures must be logged"
    );
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
