//! PostgreSQL driver for sqlr: cursor-streamed results, bound parameters, column origins,
//! table descriptions, DDL, server notices and TLS through rustls.

mod admin;
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
mod snapshot;
mod syntax;
mod tls;
mod types;

pub use connect::{PgConnection, PostgresDriver};
pub use dialect::PgDialect;
pub use syntax::{PG_KEYWORDS, PG_RESERVED, pg_syntax};

pub(crate) use errors::map_error;
