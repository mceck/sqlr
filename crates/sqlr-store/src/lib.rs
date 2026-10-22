//! Persistence for sqlr: connection configs (TOML), secrets (OS keychain) and settings.

mod catalog;
mod connections;
mod db;
mod er_layouts;
mod history;
mod keymap;
mod layered;
mod profiles;
mod recent;
mod secrets;
mod settings;
mod snippets;
mod tasks;

use std::path::{Path, PathBuf};

pub use catalog::StoredCatalog;
pub use connections::{ConnectionRepo, MoveTarget};
pub use db::{Db, HISTORY_KEEP, MAX_HISTORY_SQL_BYTES};
pub use keymap::StoredKeymap;
pub use layered::{LayeredSettings, LoadNotice};
pub use profiles::ProfileRepo;
pub use secrets::{KeyringStore, MemorySecretStore, SecretStore};
pub use settings::{
    CONNECT_TIMEOUTS_S, MONO_FONT_SIZES, PAGE_SIZES, SIDECAR_IDLE_MINUTES, Settings, UI_SCALES,
};
pub use tasks::{MAX_RUN_LOG_BYTES, RUNS_KEPT};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("config file: {0}")]
    Parse(String),
    #[error("keychain: {0}")]
    Secret(String),
    #[error("database: {0}")]
    Db(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("dev", "sqlr", "sqlr")
}

pub fn default_config_dir() -> Option<PathBuf> {
    project_dirs().map(|d| d.config_dir().to_path_buf())
}

/// Directory for logs and other non-config data.
pub fn default_data_dir() -> Option<PathBuf> {
    project_dirs().map(|d| d.data_dir().to_path_buf())
}

/// `default_data_dir()/sqlr.db`: history, snippets and the catalog cache.
pub fn default_db_path() -> Option<PathBuf> {
    default_data_dir().map(|d| d.join("sqlr.db"))
}

/// Writes `text` to `path` through a temp file in the same directory and a rename.
pub(crate) fn write_atomic(path: &Path, text: &str) -> Result<(), StoreError> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
