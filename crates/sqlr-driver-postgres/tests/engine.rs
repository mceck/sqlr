//! The object editors' engine path against PostgreSQL 17.

// One helper for every driver suite.
#[path = "../../sqlr-engine/tests/drivers/engine_objects.rs"]
mod engine_objects;
use crate::support;

use sqlr_driver_postgres::PostgresDriver;

#[test]
fn engine_creates_alters_and_drops_a_table() {
    sqlr_testkit::skip_container_tests!("POSTGRES");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (container, cfg, sec) = rt.block_on(support::pg());
    engine_objects::create_alter_drop(PostgresDriver::new(), cfg, sec, "public", "integer");
    rt.block_on(async move { drop(container) });
}
