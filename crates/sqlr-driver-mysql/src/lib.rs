//! MySQL and MariaDB driver for sqlr (`mysql_async` over rustls).

mod admin;
mod browse;
mod catalog;
mod connect;
mod ddl_gen;
mod dialect;
mod errors;
mod execute;
mod introspect;
mod manifest;
mod snapshot;
mod syntax;
mod types;

pub use connect::{MyConnection, MysqlDriver};
pub use dialect::MyDialect;
pub use syntax::{MYSQL_KEYWORDS, MYSQL_RESERVED, mysql_syntax};

pub(crate) use errors::map_error;
