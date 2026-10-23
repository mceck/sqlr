//! Native SQL Server driver for sqlr (`tiberius` over rustls).

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

pub use connect::{MsConnection, SqlServerDriver};
pub use dialect::MsDialect;
pub use syntax::{TSQL_FUNCTIONS, TSQL_KEYWORDS, TSQL_RESERVED, tsql_syntax};

pub(crate) use errors::map_error;
