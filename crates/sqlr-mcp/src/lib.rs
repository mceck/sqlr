//! sqlr's MCP server: a loopback-only Streamable HTTP endpoint while the application runs.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod discovery;
pub mod error;
pub mod guide;
pub mod host;
pub mod policy;
pub mod registry;
pub mod results;
pub mod server;
pub mod state;
pub mod token;
pub mod tools;
