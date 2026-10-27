use std::path::Path;

use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags};

use crate::StoreError;

/// Newest history rows kept by [`Db::trim_history`].
pub const HISTORY_KEEP: usize = 100_000;
/// History SQL longer than this is truncated.
pub const MAX_HISTORY_SQL_BYTES: usize = 1 << 20;

const SCHEMA_VERSION: i64 = 6;

/// Version 2: when each connection was last used (the welcome screen's "Recent").
const SCHEMA_V2: &str = "
CREATE TABLE recent_connections (conn_id TEXT PRIMARY KEY, used_at_ms INTEGER NOT NULL);
";

/// Version 3: the server database a history entry ran on, so a restored tab reconnects to it.
const SCHEMA_V3: &str = "ALTER TABLE history ADD COLUMN database TEXT;";

/// Version 4: saved tasks and their runs.
const SCHEMA_V4: &str = "
CREATE TABLE tasks (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, spec_json TEXT NOT NULL,
  created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL);
CREATE TABLE task_runs (id INTEGER PRIMARY KEY, task_id TEXT NOT NULL, started_ms INTEGER NOT NULL,
  ended_ms INTEGER, outcome TEXT NOT NULL, summary TEXT NOT NULL, log TEXT NOT NULL);
CREATE INDEX task_runs_task ON task_runs(task_id, started_ms DESC);
";

/// Version 5: ER diagram layouts.
const SCHEMA_V5: &str =
    "CREATE TABLE er_layouts (layout_key TEXT PRIMARY KEY, layout TEXT NOT NULL);";

/// Version 6: saved tasks are removed, with their runs.
const SCHEMA_V6: &str = "
DROP INDEX IF EXISTS task_runs_task;
DROP TABLE IF EXISTS task_runs;
DROP TABLE IF EXISTS tasks;
";

const SCHEMA: &str = "
CREATE TABLE history (id INTEGER PRIMARY KEY, conn_id TEXT NOT NULL, sql TEXT NOT NULL, started_at INTEGER NOT NULL,
  duration_ms INTEGER NOT NULL, outcome TEXT NOT NULL, count INTEGER, error TEXT);
CREATE INDEX history_time ON history(started_at DESC);
CREATE INDEX history_conn_time ON history(conn_id, started_at DESC);
CREATE TABLE snippets (id TEXT PRIMARY KEY, name TEXT NOT NULL, folder TEXT NOT NULL, conn_id TEXT, sql TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE catalog_schemas (conn_id TEXT PRIMARY KEY, body TEXT NOT NULL, saved_at INTEGER NOT NULL);
CREATE TABLE catalog_schema (conn_id TEXT NOT NULL, schema TEXT NOT NULL, body TEXT NOT NULL, saved_at INTEGER NOT NULL, PRIMARY KEY (conn_id, schema));
";

pub(crate) fn db_err(e: impl std::fmt::Display) -> StoreError {
    StoreError::Db(e.to_string())
}

/// The local SQLite database (`sqlr.db`). Never holds secrets.
pub struct Db {
    pub(crate) conn: Mutex<Connection>,
    /// Newest history rows [`Self::trim_history`] keeps.
    keep: usize,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_with_keep(path, HISTORY_KEEP)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        Self::init(conn, HISTORY_KEEP)
    }

    #[doc(hidden)]
    pub fn open_with_keep(path: &Path, keep: usize) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(db_err)?;
        // History can contain SQL typed before the redaction rules existed. Keep the file
        // private even when the user's umask is permissive.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(path)?.permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(path, permissions)?;
        }
        Self::init(conn, keep)
    }

    /// Opens an existing database without write access; every write fails.
    #[doc(hidden)]
    pub fn open_read_only(path: &Path) -> Result<Self, StoreError> {
        let conn =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(db_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
            keep: HISTORY_KEEP,
        })
    }

    fn init(mut conn: Connection, keep: usize) -> Result<Self, StoreError> {
        conn.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(db_err)?;
        // In-memory databases report "memory" here; that is fine.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err)?;
        // Deleted history rows are overwritten rather than left in free pages.
        conn.pragma_update(None, "secure_delete", "ON")
            .map_err(db_err)?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            keep,
        })
    }

    /// Deletes all but the newest history rows (the `keep` given when opening, else
    /// [`HISTORY_KEEP`]). It can take a while on a large history, so it is not part of
    /// opening: the engine runs it as its first background job.
    pub fn trim_history(&self) -> Result<(), StoreError> {
        let conn = self.conn.lock();
        // Old builds recorded password-bearing SQL verbatim. Remove those rows and old
        // snippets on startup; new writes are filtered before they reach the store.
        const SECRET_SQL: &str = "lower(sql) LIKE '%password%' \
             OR lower(sql) LIKE '%passwd%' \
             OR lower(sql) LIKE '%pwd%' \
             OR lower(sql) LIKE '%api_key%' \
             OR lower(sql) LIKE '%identified by%'";
        conn.execute(&format!("DELETE FROM history WHERE {SECRET_SQL}"), [])
            .map_err(db_err)?;
        conn.execute(&format!("DELETE FROM snippets WHERE {SECRET_SQL}"), [])
            .map_err(db_err)?;
        conn.execute(
            "DELETE FROM history WHERE id NOT IN \
             (SELECT id FROM history ORDER BY started_at DESC, id DESC LIMIT ?1)",
            [i64::try_from(self.keep).unwrap_or(i64::MAX)],
        )
        .map(drop)
        .map_err(db_err)
    }
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    // IMMEDIATE so two instances starting together serialize on creating the schema.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(db_err)?;
    let version: i64 = tx
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(db_err)?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::Db("newer database version".into()));
    }
    if version < 1 {
        tx.execute_batch(SCHEMA).map_err(db_err)?;
    }
    if version < 2 {
        tx.execute_batch(SCHEMA_V2).map_err(db_err)?;
    }
    if version < 3 {
        tx.execute_batch(SCHEMA_V3).map_err(db_err)?;
    }
    if version < 4 {
        tx.execute_batch(SCHEMA_V4).map_err(db_err)?;
    }
    if version < 5 {
        tx.execute_batch(SCHEMA_V5).map_err(db_err)?;
    }
    if version < 6 {
        tx.execute_batch(SCHEMA_V6).map_err(db_err)?;
    }
    if version < SCHEMA_VERSION {
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(db_err)?;
    }
    tx.commit().map_err(db_err)
}
