//! Persistence for sqlr: connection configs (TOML), encrypted secrets and settings.

mod catalog;
pub mod client_migration;
mod connections;
mod db;
mod er_layouts;
mod history;
mod keymap;
mod layered;
mod mcp_token;
mod profiles;
mod recent;
mod secrets;
mod settings;
mod snippets;
mod tasks;
mod vault;

use std::path::{Path, PathBuf};

pub use catalog::StoredCatalog;
pub use connections::{ConnectionRepo, MoveTarget};
pub use db::{Db, HISTORY_KEEP, MAX_HISTORY_SQL_BYTES};
pub use keymap::StoredKeymap;
pub use layered::{LayeredSettings, LoadNotice};
pub use mcp_token::{
    KeyringMcpTokenStore, MCP_TOKEN_ACCOUNT, MCP_TOKEN_SERVICE, McpTokenStore, MemoryMcpTokenStore,
};
pub use profiles::ProfileRepo;
pub use secrets::{KeyringStore, MemorySecretStore, SecretStore};
pub use settings::{
    CONNECT_TIMEOUTS_S, DEFAULT_MCP_PORT, MCP_PORTS, MONO_FONT_SIZES, PAGE_SIZES,
    SIDECAR_IDLE_MINUTES, Settings, TERMINAL_SCROLLBACK, UI_SCALES,
};
pub use tasks::{MAX_RUN_LOG_BYTES, RUNS_KEPT};
pub use vault::{VaultMcpTokenStore, VaultSecretStore};

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
///
/// The temp name is unique per process and write: two saves of the same file (the
/// preferences window and the soft-wrap toggle) must not share a temp file.
pub(crate) fn write_atomic(path: &Path, text: &str) -> Result<(), StoreError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TMP: AtomicU64 = AtomicU64::new(0);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT_TMP.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = PathBuf::from(tmp);
    if let Err(e) = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two saves of the same file (the preferences window and the soft-wrap toggle) must
    /// each write a whole file: a shared `.tmp` name lets their bytes interleave.
    #[test]
    fn concurrent_atomic_writes_leave_one_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let a = "a".repeat(1 << 20);
        let b = "b".repeat(1 << 20);
        for round in 0..20 {
            let (path_a, path_b) = (path.clone(), path.clone());
            let (written_a, written_b) = (a.clone(), b.clone());
            let first = std::thread::spawn(move || {
                for _ in 0..5 {
                    write_atomic(&path_a, &written_a).unwrap();
                }
            });
            let second = std::thread::spawn(move || {
                for _ in 0..5 {
                    write_atomic(&path_b, &written_b).unwrap();
                }
            });
            first.join().unwrap();
            second.join().unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text == a || text == b, "round {round}: mixed content");
            let leftovers: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .filter(|name| name != "settings.toml")
                .collect();
            assert!(leftovers.is_empty(), "round {round}: {leftovers:?}");
        }
    }
}
