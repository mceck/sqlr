//! Key-value state: `SCAN` cursors per pattern, value pages, edits and the console.
//!
//! Everything runs on the connection's explorer session, in the engine's runtime: the UI
//! asks (`kv_scan`, `kv_open`, `kv_edit`, `kv_command`) and reads snapshots
//! (`kv_keys`, `kv_value`, `kv_console`) each frame. Values load one page at a time; keys
//! load by `SCAN` page ("load more"). A late answer to a superseded request is dropped
//! (every scan and value carries a generation).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;
use sqlr_core::{
    Connection, DriverError, ErrorKind, KeyInfo, KeyType, KeyValueConn, KvCursor, KvEdit, KvPage,
    SCAN_COUNT, VALUE_PAGE,
};

use crate::engine::{Command, Engine, Shared, not_connected};
use crate::job::{JobControl, JobId, JobKind, JobOutcome};
use crate::session::SessionKey;

mod console;

use console::Console;
pub use console::ConsoleSnapshot;

/// The keys a pattern matched so far, as the UI reads them.
#[derive(Clone, Debug, Default)]
pub struct KvKeys {
    pub keys: Arc<Vec<KeyInfo>>,
    /// The `SCAN` has more to return (`kv_scan(…, more = true)` loads it).
    pub more: bool,
    pub loading: bool,
    pub error: Option<DriverError>,
}

/// A key's value as the UI reads it: its info and the pages loaded so far.
#[derive(Clone, Debug, Default)]
pub struct KvValue {
    pub info: Option<KeyInfo>,
    pub page: Option<Arc<KvPage>>,
    pub loading: bool,
    pub error: Option<DriverError>,
    /// The key does not exist (never did, or was deleted).
    pub missing: bool,
    /// The key was renamed to this name.
    pub renamed_to: Option<Vec<u8>>,
}

/// Finished key-value jobs kept for [`Engine::job`].
const KEPT_JOBS: usize = 64;

/// Patterns of one database a connection keeps scanned keys for, at most; the least recently
/// used go first.
const MAX_PATTERNS: usize = 8;

#[derive(Default)]
struct Scan {
    /// When the scan was last asked for, by the connection's tick.
    used: u64,
    keys: Arc<Vec<KeyInfo>>,
    seen: HashSet<Vec<u8>>,
    cursor: u64,
    more: bool,
    loading: bool,
    error: Option<DriverError>,
    generation: u64,
}

#[derive(Default)]
struct ConnKv {
    scans: HashMap<(u32, String), Scan>,
    values: HashMap<(u32, Vec<u8>), ValueState>,
    consoles: HashMap<u64, Console>,
    tick: u64,
}

impl ConnKv {
    /// Drops the least recently used patterns of `db` beyond [`MAX_PATTERNS`].
    fn trim_scans(&mut self, db: u32) {
        loop {
            let mut of_db: Vec<(u64, String)> = self
                .scans
                .iter()
                .filter(|((d, _), _)| *d == db)
                .map(|((_, p), s)| (s.used, p.clone()))
                .collect();
            if of_db.len() <= MAX_PATTERNS {
                return;
            }
            of_db.sort();
            self.scans.remove(&(db, of_db[0].1.clone()));
        }
    }
}

#[derive(Default)]
struct ValueState {
    info: Option<KeyInfo>,
    page: Option<Arc<KvPage>>,
    loading: bool,
    error: Option<DriverError>,
    missing: bool,
    renamed_to: Option<Vec<u8>>,
    generation: u64,
    /// The hash fields or set members loaded, so each new page is checked against them
    /// without indexing the whole value again.
    seen: HashSet<Vec<u8>>,
}

impl ValueState {
    fn snapshot(&self) -> KvValue {
        KvValue {
            info: self.info.clone(),
            page: self.page.clone(),
            loading: self.loading,
            error: self.error.clone(),
            missing: self.missing,
            renamed_to: self.renamed_to.clone(),
        }
    }
}

/// The key-value state of every connection.
pub(crate) struct KvStates {
    conns: Mutex<HashMap<String, ConnKv>>,
    scan_count: AtomicUsize,
    /// The key-value jobs that finished, oldest first.
    finished: Mutex<std::collections::VecDeque<JobId>>,
}

impl Default for KvStates {
    fn default() -> Self {
        Self {
            conns: Mutex::default(),
            scan_count: AtomicUsize::new(SCAN_COUNT),
            finished: Mutex::default(),
        }
    }
}

impl KvStates {
    /// Forgets a connection's scans, values and console.
    pub(crate) fn forget(&self, conn_id: &str) {
        self.conns.lock().remove(conn_id);
    }

    /// Remembers a finished job and forgets the oldest finished ones past [`KEPT_JOBS`].
    fn retire(&self, jobs: &crate::job::Jobs, id: JobId) {
        let mut finished = self.finished.lock();
        finished.push_back(id);
        while finished.len() > KEPT_JOBS {
            if let Some(old) = finished.pop_front() {
                jobs.forget(old);
            }
        }
    }

    fn with<R>(&self, conn_id: &str, f: impl FnOnce(&mut ConnKv) -> R) -> R {
        let mut conns = self.conns.lock();
        f(conns.entry(conn_id.to_string()).or_default())
    }
}

/// The `MATCH` pattern for what the user typed: nothing matches everything, text with no
/// glob characters matches as a prefix, anything else is the pattern.
fn scan_pattern(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        "*".into()
    } else if text.contains(['*', '?', '[']) {
        text.to_string()
    } else {
        format!("{text}*")
    }
}

impl Engine {
    /// How many keys one `SCAN` call asks for (default 500).
    pub fn set_kv_scan_count(&self, count: usize) {
        self.shared
            .kv
            .scan_count
            .store(count.max(1), Ordering::Relaxed);
    }

    /// Lets go of the scans and values loaded for database `db`, when its tab closes.
    pub fn kv_release(&self, conn_id: &str, db: u32) {
        self.shared.kv.with(conn_id, |c| {
            c.scans.retain(|(d, _), _| *d != db);
            c.values.retain(|(d, _), _| *d != db);
        });
    }

    /// The keys of `pattern` in database `db` loaded so far; `None` until it is scanned.
    pub fn kv_keys(&self, conn_id: &str, db: u32, pattern: &str) -> Option<KvKeys> {
        let conns = self.shared.kv.conns.lock();
        let scan = conns.get(conn_id)?.scans.get(&(db, pattern.to_string()))?;
        Some(KvKeys {
            keys: scan.keys.clone(),
            more: scan.more,
            loading: scan.loading,
            error: scan.error.clone(),
        })
    }

    /// Scans `pattern` (what the user typed: see the module docs). Without `more`, the scan
    /// starts over and replaces the keys when its first page arrives; with `more` it
    /// continues from the pattern's own cursor (ignored while loading or when complete).
    pub fn kv_scan(&self, conn_id: &str, db: u32, pattern: &str, more: bool) -> Option<JobId> {
        self.shared.start_scan(conn_id, db, pattern, more)
    }

    /// Deletes keys (`UNLINK`), then drops them from every loaded pattern.
    pub fn kv_delete(&self, conn_id: &str, db: u32, keys: Vec<Vec<u8>>) -> Option<JobId> {
        if keys.is_empty() {
            return None;
        }
        let conn = conn_id.to_string();
        Some(
            self.shared
                .spawn_kv(JobKind::KvEdit, move |shared, _| async move {
                    shared.run_delete(conn, db, keys).await
                }),
        )
    }

    /// Loads the key's info and first page, replacing what was loaded.
    pub fn kv_open(&self, conn_id: &str, db: u32, key: &[u8]) -> JobId {
        self.shared.start_open(conn_id, db, key)
    }

    pub fn kv_value(&self, conn_id: &str, db: u32, key: &[u8]) -> Option<KvValue> {
        let conns = self.shared.kv.conns.lock();
        let value = conns.get(conn_id)?.values.get(&(db, key.to_vec()))?;
        Some(value.snapshot())
    }

    /// Loads the page after the last one (ignored while loading, or when complete).
    pub fn kv_load_more(&self, conn_id: &str, db: u32, key: &[u8]) -> Option<JobId> {
        let more = self.shared.kv.with(conn_id, |c| {
            let state = c.values.get_mut(&(db, key.to_vec()))?;
            let from = state.page.as_ref()?.next()?;
            if state.loading {
                return None;
            }
            state.loading = true;
            state.error = None;
            state.generation += 1;
            Some((from, state.generation))
        });
        let (from, generation) = more?;
        let (conn, key) = (conn_id.to_string(), key.to_vec());
        let id = self
            .shared
            .spawn_kv(JobKind::KvValue, move |shared, _| async move {
                shared.run_more(conn, db, key, from, generation).await
            });
        self.shared.repaint();
        Some(id)
    }

    /// Applies an edit, then reloads the key. Editing a key the engine has not loaded, or
    /// renaming one, also scans the loaded patterns again.
    pub fn kv_edit(&self, conn_id: &str, db: u32, key: &[u8], edit: KvEdit) -> JobId {
        let existed = self.shared.kv.with(conn_id, |c| {
            let state = c.values.entry((db, key.to_vec())).or_default();
            state.error = None;
            state.loading = true;
            state.info.is_some()
        });
        let (conn, key) = (conn_id.to_string(), key.to_vec());
        let id = self
            .shared
            .spawn_kv(JobKind::KvEdit, move |shared, _| async move {
                shared.run_edit(conn, db, key, edit, existed).await
            });
        self.shared.repaint();
        id
    }
}

impl Shared {
    /// The connection of the explorer session, once connected.
    fn meta_connection(&self, conn_id: &str) -> Option<Arc<dyn Connection>> {
        let inner = self.inner.lock();
        let session = inner.sessions.get(&SessionKey::meta(conn_id))?;
        if session.is_connected() {
            session.conn.clone()
        } else {
            None
        }
    }

    async fn kv_connection(&self, conn_id: &str) -> Result<Arc<dyn Connection>, DriverError> {
        let io = self.live_io(conn_id).ok_or_else(not_connected)?;
        self.connected(&SessionKey::meta(conn_id), &io)
            .await
            .ok_or_else(not_connected)
    }

    fn start_scan(
        self: &Arc<Self>,
        conn_id: &str,
        db: u32,
        pattern: &str,
        more: bool,
    ) -> Option<JobId> {
        let scan = self.kv.with(conn_id, |c| {
            c.tick += 1;
            let tick = c.tick;
            let scan = c.scans.entry((db, pattern.to_string())).or_default();
            scan.used = tick;
            if more && (scan.loading || !scan.more) {
                return None;
            }
            if !more {
                scan.generation += 1;
            }
            scan.loading = true;
            scan.error = None;
            Some((if more { scan.cursor } else { 0 }, scan.generation))
        });
        self.kv.with(conn_id, |c| c.trim_scans(db));
        let (cursor, generation) = scan?;
        let (conn, pattern) = (conn_id.to_string(), pattern.to_string());
        let id = self.spawn_kv(JobKind::KvScan, move |shared, _| async move {
            shared
                .run_scan(conn, db, pattern, cursor, generation, !more)
                .await
        });
        self.repaint();
        Some(id)
    }

    /// Scans every loaded pattern of `db` again.
    fn restart_scans(self: &Arc<Self>, conn_id: &str, db: u32) {
        let patterns: Vec<String> = self.kv.with(conn_id, |c| {
            c.scans
                .keys()
                .filter(|(d, _)| *d == db)
                .map(|(_, p)| p.clone())
                .collect()
        });
        for pattern in patterns {
            self.start_scan(conn_id, db, &pattern, false);
        }
    }

    fn start_open(self: &Arc<Self>, conn_id: &str, db: u32, key: &[u8]) -> JobId {
        let generation = self.kv.with(conn_id, |c| {
            // A tab shows one key of a database: the values of the others are let go.
            c.values.retain(|(d, k), _| *d != db || k == key);
            let state = c.values.entry((db, key.to_vec())).or_default();
            state.generation += 1;
            state.loading = true;
            state.error = None;
            state.renamed_to = None;
            state.generation
        });
        let (conn, key) = (conn_id.to_string(), key.to_vec());
        let id = self.spawn_kv(JobKind::KvValue, move |shared, _| async move {
            shared.load_value(&conn, db, &key, generation).await
        });
        self.repaint();
        id
    }

    /// Runs `work` as a job of the engine's general job system: it starts on the runtime, and
    /// the job ends [`JobOutcome::Done`] (or `Failed`/`Cancelled`) when `work` returns. The
    /// state the UI reads lives in [`KvStates`]; the jobs of the last finished requests are
    /// kept for [`Engine::job`] and the rest are forgotten.
    pub(crate) fn spawn_kv<F, Fut>(self: &Arc<Self>, kind: JobKind, work: F) -> JobId
    where
        F: FnOnce(Arc<Shared>, JobControl) -> Fut,
        Fut: std::future::Future<Output = Result<(), DriverError>> + Send + 'static,
    {
        let job = self.jobs.create(kind);
        let id = job.id;
        let shared = self.clone();
        let task = work(shared.clone(), job);
        self.send(Command::job(id, None, async move {
            let outcome = match task.await {
                Ok(()) => JobOutcome::Done,
                Err(e) if e.kind == ErrorKind::Cancelled => JobOutcome::Cancelled,
                Err(e) => JobOutcome::Failed(e),
            };
            shared.jobs.finish(id, outcome);
            shared.kv.retire(&shared.jobs, id);
        }));
        id
    }

    async fn run_scan(
        self: Arc<Self>,
        conn: String,
        db: u32,
        pattern: String,
        cursor: u64,
        generation: u64,
        restart: bool,
    ) -> Result<(), DriverError> {
        let count = self.kv.scan_count.load(Ordering::Relaxed);
        let outcome = async {
            let connection = self.kv_connection(&conn).await?;
            let kv = key_value(&connection)?;
            kv.scan(db, scan_pattern(&pattern).as_bytes(), cursor, count)
                .await
        }
        .await;
        let result = outcome.as_ref().map(|_| ()).map_err(Clone::clone);
        self.kv.with(&conn, |c| {
            let Some(scan) = c.scans.get_mut(&(db, pattern.clone())) else {
                return;
            };
            if scan.generation != generation {
                return;
            }
            scan.loading = false;
            match outcome {
                Ok((found, next)) => {
                    if restart {
                        scan.seen.clear();
                        scan.keys = Arc::default();
                    }
                    let keys = Arc::make_mut(&mut scan.keys);
                    for key in found {
                        if scan.seen.insert(key.name.clone()) {
                            keys.push(key);
                        }
                    }
                    scan.cursor = next;
                    scan.more = next != 0;
                }
                Err(e) => scan.error = Some(e),
            }
        });
        self.repaint();
        result
    }

    /// Loads a key's info and first page into its value state, if `generation` is still
    /// the latest.
    async fn load_value(
        &self,
        conn: &str,
        db: u32,
        key: &[u8],
        generation: u64,
    ) -> Result<(), DriverError> {
        let outcome = async {
            let connection = self.kv_connection(conn).await?;
            let kv = key_value(&connection)?;
            let Some(info) = kv.key_info(db, key).await? else {
                return Ok(None);
            };
            let page = match info.ty {
                KeyType::Json | KeyType::Other(_) => None,
                _ => Some(kv.page(db, key, KvCursor::Start, VALUE_PAGE).await?),
            };
            Ok::<_, DriverError>(Some((info, page)))
        }
        .await;
        let result = outcome.as_ref().map(|_| ()).map_err(Clone::clone);
        self.kv.with(conn, |c| {
            let Some(state) = c.values.get_mut(&(db, key.to_vec())) else {
                return;
            };
            if state.generation != generation {
                return;
            }
            state.loading = false;
            match outcome {
                Ok(None) => {
                    state.info = None;
                    state.page = None;
                    state.missing = true;
                }
                Ok(Some((info, page))) => {
                    state.info = Some(info);
                    state.seen.clear();
                    if let Some(page) = &page {
                        page.names(&mut state.seen);
                    }
                    state.page = page.map(Arc::new);
                    state.missing = false;
                }
                Err(e) => state.error = Some(e),
            }
        });
        self.repaint();
        result
    }

    async fn run_more(
        self: Arc<Self>,
        conn: String,
        db: u32,
        key: Vec<u8>,
        from: KvCursor,
        generation: u64,
    ) -> Result<(), DriverError> {
        let outcome = async {
            let connection = self.kv_connection(&conn).await?;
            let kv = key_value(&connection)?;
            kv.page(db, &key, from, VALUE_PAGE).await
        }
        .await;
        let result = outcome.as_ref().map(|_| ()).map_err(Clone::clone);
        self.kv.with(&conn, |c| {
            let Some(state) = c.values.get_mut(&(db, key.clone())) else {
                return;
            };
            if state.generation != generation {
                return;
            }
            state.loading = false;
            match outcome {
                Ok(next) => match state.page.as_mut() {
                    Some(page) => Arc::make_mut(page).append_dedup(next, &mut state.seen),
                    None => {
                        state.seen.clear();
                        next.names(&mut state.seen);
                        state.page = Some(Arc::new(next));
                    }
                },
                Err(e) => state.error = Some(e),
            }
        });
        self.repaint();
        result
    }

    async fn run_edit(
        self: Arc<Self>,
        conn: String,
        db: u32,
        key: Vec<u8>,
        edit: KvEdit,
        existed: bool,
    ) -> Result<(), DriverError> {
        let outcome = async {
            let connection = self.kv_connection(&conn).await?;
            key_value(&connection)?.edit(db, &key, edit.clone()).await
        }
        .await;
        if let Err(e) = outcome {
            self.kv.with(&conn, |c| {
                if let Some(state) = c.values.get_mut(&(db, key.clone())) {
                    state.loading = false;
                    state.error = Some(e.clone());
                }
            });
            self.repaint();
            return Err(e);
        }
        match edit {
            KvEdit::Delete => {
                self.kv.with(&conn, |c| {
                    forget_key(c, db, &key);
                    if let Some(state) = c.values.get_mut(&(db, key.clone())) {
                        state.loading = false;
                        state.info = None;
                        state.page = None;
                        state.missing = true;
                    }
                });
            }
            KvEdit::Rename(new) => {
                self.kv.with(&conn, |c| {
                    forget_key(c, db, &key);
                    if let Some(state) = c.values.get_mut(&(db, key.clone())) {
                        state.loading = false;
                        state.info = None;
                        state.page = None;
                        state.missing = true;
                        state.renamed_to = Some(new.clone());
                    }
                });
                // The tab sees `renamed_to` and opens the new name.
                self.restart_scans(&conn, db);
            }
            _ => {
                let generation = self.kv.with(&conn, |c| {
                    let state = c.values.entry((db, key.clone())).or_default();
                    state.generation += 1;
                    state.generation
                });
                if !existed {
                    self.restart_scans(&conn, db);
                }
                let loaded = self.load_value(&conn, db, &key, generation).await;
                self.repaint();
                return loaded;
            }
        }
        self.repaint();
        Ok(())
    }

    async fn run_delete(
        self: Arc<Self>,
        conn: String,
        db: u32,
        keys: Vec<Vec<u8>>,
    ) -> Result<(), DriverError> {
        let mut failed: Option<DriverError> = None;
        let mut deleted = Vec::new();
        match self.kv_connection(&conn).await {
            Err(e) => failed = Some(e),
            Ok(connection) => match key_value(&connection) {
                Err(e) => failed = Some(e),
                Ok(kv) => {
                    for key in keys {
                        match kv.edit(db, &key, KvEdit::Delete).await {
                            Ok(()) => deleted.push(key),
                            Err(e) => {
                                failed = Some(e);
                                break;
                            }
                        }
                    }
                }
            },
        }
        self.kv.with(&conn, |c| {
            for key in &deleted {
                forget_key(c, db, key);
                if let Some(state) = c.values.get_mut(&(db, key.clone())) {
                    state.info = None;
                    state.page = None;
                    state.missing = true;
                }
            }
        });
        self.repaint();
        match failed {
            Some(e) => {
                self.notice(format!(
                    "Could not delete the keys of {conn}: {}",
                    e.message
                ));
                Err(e)
            }
            None => Ok(()),
        }
    }
}

fn key_value(conn: &Arc<dyn Connection>) -> Result<&dyn KeyValueConn, DriverError> {
    conn.key_value().ok_or_else(|| {
        DriverError::new(
            ErrorKind::Unsupported,
            "this connection is not a key-value database",
        )
    })
}

/// Drops `key` from every loaded scan of `db`.
fn forget_key(c: &mut ConnKv, db: u32, key: &[u8]) {
    for ((d, _), scan) in c.scans.iter_mut() {
        if *d == db && scan.seen.remove(key) {
            Arc::make_mut(&mut scan.keys).retain(|k| k.name != key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_patterns_become_match_patterns() {
        assert_eq!(scan_pattern(""), "*");
        assert_eq!(scan_pattern("  "), "*");
        assert_eq!(scan_pattern("user:"), "user:*");
        assert_eq!(scan_pattern("user:*"), "user:*");
        assert_eq!(scan_pattern("a?b"), "a?b");
        assert_eq!(scan_pattern("[ab]x"), "[ab]x");
    }
}
