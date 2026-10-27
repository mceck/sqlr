//! The incident file store, the session marker/lock and the private-directory helpers.
//!
//! Nothing here touches the local SQLite database, the vault or the configuration: a broken
//! database cannot prevent a report. Files are ordinary JSON, written through a private
//! temporary file in the same directory and renamed into place.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::diagnostics::model::{Incident, IncidentKind, IncidentSummary};

/// At most this many incident files are kept.
pub const INCIDENT_FILES: usize = 20;
/// Incidents older than this are pruned.
pub const INCIDENT_DAYS: i64 = 30;

const INCIDENT_EXTENSION: &str = ".json";
const MARKER_FILE: &str = "session.json";
const LOCK_FILE: &str = "session.lock";

/// The current session marker. Its presence on the next launch means the previous session
/// did not exit cleanly.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionMarker {
    pub session_id: String,
    pub pid: u32,
    pub started_ms: i64,
    pub version: String,
}

/// The exclusive session lock and the marker file it guards.
///
/// The lock lives in `session.lock`; the marker is a separate `session.json`, written
/// atomically. Keeping them apart lets the marker be replaced by rename without moving the
/// lock to a new inode, which would let a second process believe it is alone.
pub struct SessionLock {
    /// `None` when the lock file could not be opened: diagnostics still works, but no
    /// crash-on-next-launch notice is possible.
    file: Option<File>,
    /// Whether this process owns the lock and may write the marker.
    holder: bool,
    /// Another process holds the lock.
    other: bool,
    dir: PathBuf,
}

impl SessionLock {
    pub fn acquire(dir: &Path) -> Self {
        let path = dir.join(LOCK_FILE);
        let Ok(file) = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
        else {
            return Self {
                file: None,
                holder: false,
                other: false,
                dir: dir.to_path_buf(),
            };
        };
        let _ = restrict_file(&path);
        let (holder, other) = match file.try_lock() {
            Ok(()) => (true, false),
            Err(std::fs::TryLockError::WouldBlock) => (false, true),
            Err(std::fs::TryLockError::Error(_)) => (false, false),
        };
        Self {
            file: Some(file),
            holder,
            other,
            dir: dir.to_path_buf(),
        }
    }

    /// Another process owns the diagnostics directory right now.
    pub fn other_instance(&self) -> bool {
        self.other
    }

    /// This process may write the marker.
    pub fn holder(&self) -> bool {
        self.holder
    }

    pub fn read_marker(&self) -> Option<SessionMarker> {
        if !self.holder {
            return None;
        }
        let text = std::fs::read_to_string(self.dir.join(MARKER_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn write_marker(&self, marker: &SessionMarker) -> std::io::Result<()> {
        if !self.holder {
            return Ok(());
        }
        let text = serde_json::to_string_pretty(marker)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_private_atomic(&self.dir.join(MARKER_FILE), &text)
    }

    /// Removes the marker: the next launch is clean. The lock file itself stays so the
    /// inode other processes may have opened remains valid.
    pub fn clear_marker(&self) -> std::io::Result<()> {
        if !self.holder {
            return Ok(());
        }
        match std::fs::remove_file(self.dir.join(MARKER_FILE)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
        }
    }
}

/// Creates `dir` (and its parents) and makes it readable only by its owner.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    restrict_dir(dir)
}

/// The incidents in `dir`, newest first. Unrelated and unreadable files are ignored.
pub fn list(dir: &Path) -> Vec<Incident> {
    let mut incidents = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return incidents;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_incident_name(name) {
            continue;
        }
        if let Some(incident) = read(dir, name.trim_end_matches(INCIDENT_EXTENSION)) {
            incidents.push(incident);
        }
    }
    incidents.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| b.id.cmp(&a.id)));
    incidents
}

/// Reads one incident by id. `None` when it does not exist or is not valid JSON.
pub fn read(dir: &Path, id: &str) -> Option<Incident> {
    if !valid_id(id) {
        return None;
    }
    let text = std::fs::read_to_string(dir.join(format!("{id}{INCIDENT_EXTENSION}"))).ok()?;
    serde_json::from_str(&text).ok()
}

/// Reads the incident's text exactly as written (crash-banner Copy).
pub fn read_json(dir: &Path, id: &str) -> Option<String> {
    if !valid_id(id) {
        return None;
    }
    std::fs::read_to_string(dir.join(format!("{id}{INCIDENT_EXTENSION}"))).ok()
}

/// Writes an incident atomically and prunes old ones. Returns the file path.
pub fn write(dir: &Path, incident: &Incident) -> std::io::Result<PathBuf> {
    ensure_private_dir(dir)?;
    if !valid_id(&incident.id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid incident id",
        ));
    }
    let path = dir.join(format!("{}{INCIDENT_EXTENSION}", incident.id));
    let text = serde_json::to_string_pretty(incident)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_private_atomic(&path, &text)?;
    prune(dir);
    Ok(path)
}

/// Replaces an incident's `acknowledged` flag atomically.
pub fn acknowledge(dir: &Path, id: &str) -> std::io::Result<()> {
    let Some(mut incident) = read(dir, id) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "incident not found",
        ));
    };
    if incident.acknowledged {
        return Ok(());
    }
    incident.acknowledged = true;
    write(dir, &incident)?;
    Ok(())
}

/// Keeps the newest [`INCIDENT_FILES`] incidents and drops those older than
/// [`INCIDENT_DAYS`]. Never fails a caller: pruning is best effort.
pub fn prune(dir: &Path) {
    let mut ours: Vec<(String, PathBuf, i64, SystemTime)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_incident_name(name) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        // The file name carries the UTC time; the mtime is the fallback if it is odd.
        let at = filename_ms(name).unwrap_or_else(|| {
            modified
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64)
        });
        ours.push((name.to_string(), entry.path(), at, modified));
    }
    ours.sort_by_key(|entry| std::cmp::Reverse(entry.2));
    let cutoff = now_ms() - INCIDENT_DAYS * 24 * 60 * 60 * 1000;
    for (index, (_, path, at, _)) in ours.into_iter().enumerate() {
        if index >= INCIDENT_FILES || at < cutoff {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The summaries of the incidents in `dir`, newest first.
pub fn summaries(dir: &Path) -> Vec<IncidentSummary> {
    list(dir).iter().map(Incident::summary).collect()
}

/// The newest unacknowledged incident of `kind` written by `session_id`.
pub fn find_unacknowledged(dir: &Path, kind: IncidentKind, session_id: &str) -> Option<Incident> {
    list(dir)
        .into_iter()
        .find(|i| i.kind == kind && !i.acknowledged && i.session_id == session_id)
}

fn is_incident_name(name: &str) -> bool {
    name.ends_with(INCIDENT_EXTENSION) && valid_id(name.trim_end_matches(INCIDENT_EXTENSION))
}

/// `YYYY-MM-DDTHH-MM-SSZ-pid-suffix`: the time sorts lexicographically, and the process id
/// and random suffix keep two writers at the same instant apart.
fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() < 22 {
        return false;
    }
    let digit = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
    };
    digit(0..4)
        && bytes.get(4) == Some(&b'-')
        && digit(5..7)
        && bytes.get(7) == Some(&b'-')
        && digit(8..10)
        && bytes.get(10) == Some(&b'T')
        && digit(11..13)
        && bytes.get(13) == Some(&b'-')
        && digit(14..16)
        && bytes.get(16) == Some(&b'-')
        && digit(17..19)
        && bytes.get(19) == Some(&b'Z')
        && id
            .get(20..)
            .and_then(|rest| rest.strip_prefix('-'))
            .and_then(|rest| rest.split_once('-'))
            .is_some_and(|(pid, suffix)| {
                !pid.is_empty()
                    && pid.bytes().all(|b| b.is_ascii_digit())
                    && !suffix.is_empty()
                    && suffix.bytes().all(|b| b.is_ascii_hexdigit())
            })
}

fn filename_ms(name: &str) -> Option<i64> {
    let id = name.trim_end_matches(INCIDENT_EXTENSION);
    if !valid_id(id) {
        return None;
    }
    // The id's first 20 bytes are `YYYY-MM-DDTHH-MM-SSZ`; parse it with jiff.
    let text = format!(
        "{}-{}-{}T{}:{}:{}Z",
        &id[0..4],
        &id[5..7],
        &id[8..10],
        &id[11..13],
        &id[14..16],
        &id[17..19]
    );
    let timestamp: jiff::Timestamp = text.parse().ok()?;
    Some(timestamp.as_millisecond())
}

pub fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

/// The UTC `YYYY-MM-DDTHH-MM-SSZ` prefix of an incident id.
pub fn timestamp_prefix(at_ms: i64) -> String {
    jiff::Timestamp::from_millisecond(at_ms).map_or_else(
        |_| "0000-00-00T00-00-00Z".into(),
        |t| t.strftime("%Y-%m-%dT%H-%M-%SZ").to_string(),
    )
}

/// Writes `text` through a temporary file in the same directory, with owner-only
/// permissions, then renames it into place.
pub fn write_private_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_private_dir(dir)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("diagnostics");
    let tmp = path.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        restrict_file(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    Ok(())
}

/// Writes a user-chosen file with owner-only permissions, without touching the directory's
/// permissions (the destination may be the Desktop or a shared folder).
pub fn write_user_file(path: &Path, text: &str) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    restrict_file(path)?;
    file.write_all(text.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()?;
    restrict_file(path)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(dir)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(dir, perms)
}

#[cfg(not(unix))]
fn restrict_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::model::{Environment, INCIDENT_SCHEMA};

    fn incident(id: &str, at_ms: i64, kind: IncidentKind) -> Incident {
        Incident {
            schema: INCIDENT_SCHEMA,
            id: id.into(),
            session_id: "session-a".into(),
            at_ms,
            kind,
            message: "boom".into(),
            target: None,
            thread: None,
            file: None,
            line: None,
            error_kind: None,
            fields: Default::default(),
            note: None,
            acknowledged: false,
            environment: Environment::basic(),
            logs: Vec::new(),
            backtrace: None,
        }
    }

    fn id_at(at_ms: i64, suffix: u32) -> String {
        format!("{}-4711-{suffix:x}", timestamp_prefix(at_ms))
    }

    #[test]
    fn write_read_and_acknowledge_survive_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let at = now_ms();
        let id = id_at(at, 1);
        let path = write(dir.path(), &incident(&id, at, IncidentKind::Error)).unwrap();
        assert!(path.exists());
        let loaded = list(dir.path());
        assert_eq!(loaded.len(), 1);
        assert!(!loaded[0].acknowledged);

        acknowledge(dir.path(), &id).unwrap();
        let reloaded = list(dir.path());
        assert!(reloaded[0].acknowledged);
        assert!(
            read_json(dir.path(), &id)
                .unwrap()
                .contains("\"acknowledged\": true")
        );
    }

    #[test]
    fn retention_keeps_the_newest_twenty_and_ignores_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = now_ms();
        std::fs::write(dir.path().join("notes.txt"), "not an incident").unwrap();
        for i in 0..25 {
            let at = base - i * 1_000;
            let id = id_at(at, i as u32);
            write(dir.path(), &incident(&id, at, IncidentKind::Error)).unwrap();
        }
        let kept = list(dir.path());
        assert_eq!(kept.len(), INCIDENT_FILES);
        assert!(kept[0].at_ms > kept[kept.len() - 1].at_ms);
        assert!(dir.path().join("notes.txt").exists());
    }

    #[test]
    fn retention_drops_incidents_older_than_thirty_days() {
        let dir = tempfile::tempdir().unwrap();
        let recent = now_ms();
        let old = recent - (INCIDENT_DAYS + 1) * 24 * 60 * 60 * 1000;
        let old_id = id_at(old, 1);
        let recent_id = id_at(recent, 2);
        write(dir.path(), &incident(&old_id, old, IncidentKind::Error)).unwrap();
        write(
            dir.path(),
            &incident(&recent_id, recent, IncidentKind::Error),
        )
        .unwrap();
        let kept = list(dir.path());
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, recent_id);
    }

    #[test]
    fn invalid_incident_names_are_never_read_or_written() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path(), "../session").is_none());
        assert!(read(dir.path(), "/etc/passwd").is_none());
        assert!(write(dir.path(), &incident("../escape", 0, IncidentKind::Error)).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn incident_files_and_the_directory_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let at = now_ms();
        let id = id_at(at, 3);
        let path = write(dir.path(), &incident(&id, at, IncidentKind::Error)).unwrap();
        let file = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let folder = std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(file, 0o600);
        assert_eq!(folder, 0o700);
    }

    #[test]
    fn the_session_marker_distinguishes_clean_unclean_and_a_live_instance() {
        let dir = tempfile::tempdir().unwrap();
        ensure_private_dir(dir.path()).unwrap();
        let marker = SessionMarker {
            session_id: "one".into(),
            pid: 1,
            started_ms: 2,
            version: env!("CARGO_PKG_VERSION").into(),
        };

        // A clean exit leaves no marker.
        {
            let lock = SessionLock::acquire(dir.path());
            assert!(lock.holder() && !lock.other_instance());
            lock.write_marker(&marker).unwrap();
            assert_eq!(lock.read_marker(), Some(marker.clone()));
            lock.clear_marker().unwrap();
        }
        {
            let lock = SessionLock::acquire(dir.path());
            assert!(lock.holder());
            assert_eq!(lock.read_marker(), None, "clean exit");
        }

        // Leaving a marker behind is an unclean exit.
        {
            let lock = SessionLock::acquire(dir.path());
            lock.write_marker(&marker).unwrap();
        }
        {
            let lock = SessionLock::acquire(dir.path());
            assert_eq!(lock.read_marker(), Some(marker.clone()));
            lock.clear_marker().unwrap();
        }

        // A second live instance sees the first one's lock and does not touch the marker.
        {
            let first = SessionLock::acquire(dir.path());
            first.write_marker(&marker).unwrap();
            let second = SessionLock::acquire(dir.path());
            assert!(!second.holder());
            assert!(second.other_instance());
            assert_eq!(second.read_marker(), None);
            second.clear_marker().unwrap();
            assert_eq!(first.read_marker(), Some(marker));
            first.clear_marker().unwrap();
        }
    }

    #[test]
    fn find_unacknowledged_matches_the_kind_and_session() {
        let dir = tempfile::tempdir().unwrap();
        let at = now_ms();
        let id = id_at(at, 4);
        let mut panic = incident(&id, at, IncidentKind::Panic);
        panic.session_id = "old".into();
        write(dir.path(), &panic).unwrap();
        assert!(find_unacknowledged(dir.path(), IncidentKind::Panic, "old").is_some());
        assert!(find_unacknowledged(dir.path(), IncidentKind::Panic, "new").is_none());
        acknowledge(dir.path(), &id).unwrap();
        assert!(find_unacknowledged(dir.path(), IncidentKind::Panic, "old").is_none());
    }

    #[test]
    fn timestamp_prefix_round_trips() {
        let at = 1_790_000_000_000;
        let id = format!("{}-1-a", timestamp_prefix(at));
        assert_eq!(filename_ms(&format!("{id}.json")), Some(at));
    }

    #[test]
    fn pruning_never_fails_on_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        prune(&dir.path().join("nope"));
    }
}
