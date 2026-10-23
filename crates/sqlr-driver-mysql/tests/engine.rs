//! The object editors' engine path against MySQL 8.4 and MariaDB 11.4.

// One helper for every driver suite.
#[path = "../../sqlr-engine/tests/drivers/engine_objects.rs"]
mod engine_objects;
mod support;

use sqlr_driver_mysql::MysqlDriver;
use support::Server;

fn create_alter_drop(server: Server) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (container, cfg, sec) = rt.block_on(support::start(server));
    let database = cfg.values["database"].clone();
    engine_objects::create_alter_drop(MysqlDriver::new(), cfg, sec, &database, "int");
    rt.block_on(async move { drop(container) });
}

#[test]
fn engine_creates_alters_and_drops_a_table_mysql() {
    sqlr_testkit::skip_container_tests!();
    create_alter_drop(Server::MySql);
}

#[test]
fn engine_creates_alters_and_drops_a_table_mariadb() {
    sqlr_testkit::skip_container_tests!();
    create_alter_drop(Server::MariaDb);
}
