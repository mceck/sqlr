//! The native ClickHouse driver through the engine's public session interface.
use crate::support::{TIMEOUT, config, dev, no_repaint, wait_until};
use sqlr_core::{Cell, NodePath, Secrets};
use sqlr_engine::{DriverRegistry, Engine, LoadState, QueryStatus, SessionStatus};

#[test]
fn native_session_fetches_pages_and_browses_the_catalog() {
    sqlr_testkit::skip_container_tests!("CLICKHOUSE");
    let (host, port) = sqlr_testkit::service("CLICKHOUSE");
    let mut drivers = DriverRegistry::default();
    drivers.register(sqlr_driver_clickhouse::ClickHouseDriver::new());
    let engine = Engine::new(drivers, no_repaint());
    let mut cfg = config("clickhouse", "clickhouse");
    cfg.values = [
        ("host".into(), host),
        ("port".into(), port.to_string()),
        ("user".into(), "sqlr".into()),
    ]
    .into();
    engine.connect(
        cfg,
        Secrets([("password".into(), "sqlr-test".into())].into()),
        dev(),
    );
    assert!(
        wait_until(TIMEOUT, || matches!(
            engine.session_status("clickhouse"),
            Some(SessionStatus::Connected(_))
        )),
        "{:?}",
        engine.session_status("clickhouse")
    );
    let query = engine.execute(
        "clickhouse",
        "SELECT toInt64(number) AS n FROM numbers(7)".into(),
        3,
    );
    for (index, expected) in [3, 6, 7].into_iter().enumerate() {
        if index > 0 {
            engine.fetch_more(query);
        }
        assert!(wait_until(TIMEOUT, || engine
            .result(query)
            .is_some_and(|r| r.read().status != QueryStatus::Running)));
        if index == 0 {
            let root = NodePath::root();
            engine.browse("clickhouse", root.clone());
            assert!(wait_until(TIMEOUT, || matches!(
                engine.children("clickhouse", &root),
                Some(LoadState::Ready(_) | LoadState::Failed(_))
            )));
            assert!(
                matches!(
                    engine.children("clickhouse", &root),
                    Some(LoadState::Ready(_))
                ),
                "{:?}",
                engine.children("clickhouse", &root)
            );
        }
        let result = engine.result(query).unwrap();
        let result = result.read();
        assert!(
            matches!(result.status, QueryStatus::Ready { .. }),
            "{:?}",
            result.status
        );
        assert_eq!(result.rows.row_count(), expected);
        assert_eq!(
            result.rows.cell(expected - 1, 0),
            Some(Cell::Int(expected as i64 - 1))
        );
    }
    engine.disconnect("clickhouse");
}
