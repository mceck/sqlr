//! The preferences window and the resolution of layered settings for the rest of the app.

mod resolve;
mod sections;
pub(crate) mod terminal;
mod window;

pub(crate) use resolve::{Formats, Scopes, conn_key, engine_overrides, tightened};
pub(crate) use window::PrefsWindow;
