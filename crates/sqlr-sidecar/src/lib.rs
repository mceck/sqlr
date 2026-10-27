//! Out-of-process drivers (spec §6): the sidecar protocol, the client that makes a child
//! process look like a [`sqlr_core::Driver`], and the server loop drivers use to answer.
//!
//! See `docs/driver-protocol.md` for the wire format.

#![cfg_attr(not(test), warn(clippy::unwrap_used))]
#![warn(missing_docs)]

pub mod dialect;
pub mod frame;
pub mod messages;
pub mod server;

mod client;
mod process;

pub use client::{SidecarConnection, SidecarDriver};
pub use dialect::{DialectSpec, LimitStyle, PlaceholderStyle, SidecarDialect, SyntaxSpec};
pub use frame::{Frame, FrameError, MAX_FRAME};
pub use messages::{PROTOCOL, Request, Response};
pub use process::SidecarSpec;
pub use server::{serve, serve_arc, serve_io};
