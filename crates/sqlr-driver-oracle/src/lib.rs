//! Native Oracle driver for sqlr (`oracle-rs`, pure Rust, no Instant Client).

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod browse;
mod catalog;
mod connect;
mod ddl_gen;
mod describe;
mod dialect;
mod errors;
mod execute;
mod introspect;
mod manifest;
mod syntax;
mod types;

pub use connect::{OracleConnection, OracleDriver};
pub use dialect::OracleDialect;
pub use syntax::{ORACLE_FUNCTIONS, ORACLE_KEYWORDS, ORACLE_RESERVED, oracle_syntax};

pub(crate) use errors::map_error;
