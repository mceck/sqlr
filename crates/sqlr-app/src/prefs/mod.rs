//! The preferences window and the resolution of layered settings for the rest of the app.

mod resolve;
mod sections;
mod window;

pub(crate) use resolve::{Formats, Scopes, conn_key, engine_overrides, tightened};
pub(crate) use window::PrefsWindow;
