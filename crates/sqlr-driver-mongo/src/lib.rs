//! MongoDB driver for sqlr (official `mongodb` crate over rustls with the ring provider).
//!
//! Queries are the mongo shell subset parsed by `sqlr_core::parse_mongo_shell`; each
//! statement runs as a raw command built from `DocValue`s, so documents keep their key order.

#![cfg_attr(not(test), warn(clippy::unwrap_used))]

mod browse;
mod connect;
mod ejson;
mod errors;
mod manifest;
mod query;
mod write;

pub use connect::{MongoConnection, MongoDriver};
