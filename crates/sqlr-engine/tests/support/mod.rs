#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these helpers"
)]

//! Helpers shared by the engine integration tests.

use std::time::{Duration, Instant};

use sqlr_core::ConnectionConfig;

pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Polls `cond` every 5 ms until it holds or `timeout` elapses.
pub fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn config(id: &str, driver: &str) -> ConnectionConfig {
    ConnectionConfig {
        id: id.into(),
        name: id.into(),
        driver: driver.into(),
        group: None,
        color: None,
        values: Default::default(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    }
}

pub fn no_repaint() -> sqlr_engine::Repaint {
    std::sync::Arc::new(|| {})
}

/// The built-in development profile (auto-commit, writable).
pub fn dev() -> sqlr_core::ConnectionProfile {
    sqlr_core::ConnectionProfile::development()
}

/// A fixed test service endpoint (`sqlr-testkit` defaults to the compose ports).
pub fn endpoint(name: &str) -> (String, u16) {
    sqlr_testkit::service(name)
}

/// Drops every non-system Mongo database before a test that uses fixed names.
pub async fn reset_mongo(host: &str, port: u16) {
    let client = mongodb::Client::with_uri_str(format!("mongodb://{host}:{port}/"))
        .await
        .expect("connect mongo");
    for database in client
        .list_database_names()
        .await
        .expect("list mongo databases")
    {
        if !matches!(database.as_str(), "admin" | "local" | "config") {
            client
                .database(&database)
                .drop()
                .await
                .unwrap_or_else(|e| panic!("drop mongo {database}: {e:?}"));
        }
    }
}

/// Deletes Elasticsearch indices, data streams and templates before a fixed-name test.
pub async fn reset_elastic(host: &str, port: u16) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::new();
    let base = format!("http://{host}:{port}");
    for path in ["_data_stream/*", "_index_template/*", "_all"] {
        let response = client
            .delete(format!("{base}/{path}"))
            .send()
            .await
            .expect("reset elasticsearch");
        assert!(
            response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND,
            "reset {path}: {}",
            response.status()
        );
    }
}

/// Clears every Redis logical database before a fixed-name test.
pub async fn reset_redis(host: &str, port: u16) {
    let client = redis::Client::open(format!("redis://{host}:{port}/")).unwrap();
    for attempt in 0..3 {
        let mut conn = match client.get_multiplexed_async_connection().await {
            Ok(conn) => conn,
            Err(_) if attempt < 2 => {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                continue;
            }
            Err(e) => panic!("connect redis: {e}"),
        };
        match redis::cmd("FLUSHALL")
            .arg("ASYNC")
            .query_async::<redis::Value>(&mut conn)
            .await
        {
            Ok(_) => return,
            Err(_) if attempt < 2 => {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            Err(e) => panic!("flush redis: {e}"),
        }
    }
    panic!("flush redis: timed out");
}

/// MongoDB through the same client used for an installed sidecar.
pub fn mongo_sidecar() -> std::sync::Arc<dyn sqlr_core::Driver> {
    use sqlr_core::Driver;
    std::sync::Arc::new(sqlr_sidecar::SidecarDriver::with_manifest(
        sqlr_sidecar::SidecarSpec::new(
            "mongodb",
            env!("CARGO_PKG_VERSION"),
            sqlr_testkit::workspace_binary("sqlr-driver-mongodb"),
        ),
        sqlr_driver_mongo::MongoDriver::new().manifest().clone(),
    ))
}
