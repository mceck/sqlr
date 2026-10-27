//! File access for agent installations: reads, the one-time backup and atomic writes that
//! keep the file's permissions.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// The backup's suffix, next to the original file.
pub(super) const BACKUP_SUFFIX: &str = ".sqlr-backup";

/// The file's text, or `None` when it does not exist.
pub(super) fn read(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(BACKUP_SUFFIX);
    PathBuf::from(name)
}

/// Copies `path` to `<path>.sqlr-backup` unless a backup already exists, so the copy is
/// always the file as it was before sqlr first changed it.
pub(super) fn backup_once(path: &Path) -> io::Result<()> {
    let backup = backup_path(path);
    if backup.symlink_metadata().is_ok() {
        return Ok(());
    }
    std::fs::copy(path, &backup).map(|_| ())
}

/// Writes through a temporary file in the same directory and a rename. An existing file
/// keeps its permissions; a new one is owner-only on Unix. A symbolic link (a dotfiles
/// manager's) is followed, so the link stays and its target changes.
pub(super) fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let target = if path.symlink_metadata().is_ok_and(|meta| meta.is_symlink()) {
        std::fs::canonicalize(path)?
    } else {
        path.to_path_buf()
    };
    let dir = target
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let permissions = std::fs::metadata(&target)
        .ok()
        .map(|meta| meta.permissions());
    // `tempfile` creates the file owner-only on Unix.
    let mut temp = tempfile::Builder::new()
        .prefix(".sqlr-")
        .suffix(".tmp")
        .tempfile_in(dir)?;
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    if let Some(permissions) = permissions {
        std::fs::set_permissions(temp.path(), permissions)?;
    }
    temp.persist(&target).map_err(|error| error.error)?;
    Ok(())
}
