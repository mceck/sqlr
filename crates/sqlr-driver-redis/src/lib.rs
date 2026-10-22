//! Redis driver for sqlr (the `redis` crate over rustls with the ring provider).
//!
//! Browsing uses `SCAN` and never `KEYS`; large values page with `HSCAN`, `SSCAN`,
//! `LRANGE`, `ZRANGE` and `XRANGE`. A Redis Cluster endpoint is refused.

mod browse;
mod classify;
mod connect;
mod console;
mod errors;
mod manifest;
mod values;

pub use connect::{RedisConnection, RedisDriver};
