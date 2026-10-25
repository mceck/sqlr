//! The `mcp.json` discovery file: URL, process id, protocol and app version, start time.
//! It never contains the token.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ToolError, ToolResult};

pub const DISCOVERY_FILE: &str = "mcp.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Discovery {
    pub url: String,
    pub pid: u32,
    pub protocol_version: String,
    pub app_version: String,
    /// RFC 3339 / ISO 8601.
    pub started_at: String,
}

impl Discovery {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(DISCOVERY_FILE)
    }
}

/// Writes the file atomically: a temp file in the same directory, then a rename. On Unix
/// the file is mode 0600.
pub fn write(data_dir: &Path, discovery: &Discovery) -> ToolResult<PathBuf> {
    let path = Discovery::path(data_dir);
    let text = serde_json::to_string_pretty(discovery)
        .map_err(|e| ToolError::internal(format!("could not encode discovery: {e}")))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let result = (|| -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&tmp, text.as_bytes())?;
        }
        std::fs::rename(&tmp, &path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(ToolError::internal(format!(
            "could not write the discovery file: {e}"
        )));
    }
    Ok(path)
}

/// Removes the discovery file; missing is fine.
pub fn remove(data_dir: &Path) {
    let _ = std::fs::remove_file(Discovery::path(data_dir));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery() -> Discovery {
        Discovery {
            url: "http://127.0.0.1:47821/mcp".into(),
            pid: 42,
            protocol_version: "2025-11-25".into(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn write_is_atomic_private_and_has_no_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), &discovery()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("127.0.0.1:47821"));
        assert!(!text.contains("token"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        remove(dir.path());
        assert!(!path.exists());
        remove(dir.path());
    }

    #[test]
    fn a_stale_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(Discovery::path(dir.path()), "old").unwrap();
        let path = write(dir.path(), &discovery()).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("\"url\""));
        assert!(!text.contains("old"));
    }
}
