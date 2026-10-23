//! The object editors' engine path against SQLite.

// One helper for every driver suite.
#[path = "../../sqlr-engine/tests/drivers/engine_objects.rs"]
mod engine_objects;

use sqlr_core::{ConnectionConfig, Secrets};
use sqlr_driver_sqlite::SqliteDriver;

#[test]
fn engine_creates_alters_and_drops_a_table() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ConnectionConfig {
        id: "t".into(),
        name: "test".into(),
        driver: "sqlite".into(),
        group: None,
        color: None,
        values: [
            ("path", dir.path().join("t.db").display().to_string()),
            ("create_if_missing", "true".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    };
    engine_objects::create_alter_drop(
        SqliteDriver::new(),
        cfg,
        Secrets::default(),
        "main",
        "INTEGER",
    );
}
