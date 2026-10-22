//! The sqlr application shell: layout A, wiring the engine, store and widgets together.

mod actions;
pub use actions::dispatch_actions;
mod admin;
mod app;
mod catalog_source;
pub mod cli;
mod compare_dialog;
mod confirm;
mod database_tools;
mod databases_dialog;
mod datagen_dialog;
mod ddl_tab;
mod demo;
mod dialogs;
mod drivers;
mod explorer;
mod fonts;
mod help;
mod icon;
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
mod tasks_ui;
mod transaction;
mod transfer_dialogs;
mod transfer_wizard;
mod tree_snapshot;
mod welcome;

pub use app::{AppDeps, SqlrApp, production_deps, production_deps_in};
pub use library::open_db_or_memory;
pub use logging::init as init_logging;
