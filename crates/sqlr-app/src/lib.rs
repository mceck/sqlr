//! The sqlr application shell: layout A, wiring the engine, store and widgets together.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod actions;
mod agents;
pub use actions::dispatch_actions;
mod admin;
mod app;
mod catalog_picker;
mod catalog_source;
pub mod cli;
mod client_migration;
mod compare_dialog;
mod confirm;
mod database_tools;
mod databases_dialog;
mod datagen_dialog;
mod ddl_tab;
mod demo;
pub mod diagnostics;
mod dialogs;
mod drivers;
mod explorer;
mod fonts;
mod help;
mod icon;
mod job_failure;
pub use icon::window_icon;
mod library;
mod logging;
mod messages;
#[cfg(target_os = "macos")]
mod native_menu;
mod nosql;
mod objects;
mod palette;
mod prefs;
mod profiles_dialog;
mod results;
mod schema_tools;
mod shell;
mod store_worker;
mod tabs;
mod terminal_workspace;
mod transaction;
mod transfer_dialogs;
mod transfer_wizard;
mod tree_snapshot;
mod updates;
mod welcome;

pub use app::{AppDeps, SqlrApp, production_deps, production_deps_in, production_deps_with};
pub use diagnostics::{
    Diagnostics, DiagnosticsUi, Incident, IncidentKind, IncidentSummary, LogLevel, PanicHookGuard,
    StartupNotice,
};
pub use library::open_db_or_memory;
pub use logging::init as init_logging;

#[cfg(test)]
mod visual_audit;
