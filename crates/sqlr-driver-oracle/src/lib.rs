//! Native Oracle driver for sqlr (`oracle-rs`, pure Rust, no Instant Client).

mod browse;
mod catalog;
mod connect;
mod describe;
mod dialect;
mod errors;
mod execute;
mod manifest;
mod syntax;
mod types;

pub use connect::{OracleConnection, OracleDriver};
pub use dialect::OracleDialect;
pub use syntax::{ORACLE_FUNCTIONS, ORACLE_KEYWORDS, ORACLE_RESERVED, oracle_syntax};

pub(crate) use errors::map_error;
