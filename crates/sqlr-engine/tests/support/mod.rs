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
    }
}

pub fn no_repaint() -> sqlr_engine::Repaint {
    std::sync::Arc::new(|| {})
}

/// The built-in development profile (auto-commit, writable).
pub fn dev() -> sqlr_core::ConnectionProfile {
    sqlr_core::ConnectionProfile::development()
}
