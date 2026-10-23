//! The engine facade (called from the UI thread) and the runtime tasks behind it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use sqlr_core::{
    CancelHandle, CatalogNeed, Changeset, CommitMode, Connection, ConnectionConfig,
    ConnectionProfile, Driver, DriverError, DriverManifest, DumpOptions, ErrorKind, Execution,
    HistoryQuery, Node, NodePath, RestoreOptions, RowBuffer, Secrets, Snippet, TableInfo, TableRef,
    TextFormat,
};
use sqlr_store::Db;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedSender, WeakUnboundedSender, unbounded_channel};

use crate::catalog::{CatalogCache, ConnCatalog, DbQueue, run_db_worker};
use crate::job::{JobControl, JobId, JobKind, JobOutcome, JobState, Jobs};
use crate::library::{HistoryChannel, HistoryView, Library, history_entry, now_ms};
use crate::registry::DriverRegistry;
use crate::result::{QueryId, QueryResult, QueryStatus};
use crate::script::{ScriptId, ScriptOnError, ScriptRun, ScriptState, Scripts};
use crate::session::{LoadState, Session, SessionIo, SessionKey, SessionStatus, Slot, StreamSlot};
use crate::ssh::{self, ConnectReport, SshContext, TunnelSlot};
use crate::store::{ProfilesState, SharedSecretStore};
use crate::transfer::{ExportSource, ImportOptions, TransferRun, Work};
use crate::tx::{MessageLevel, READ_ONLY, RunMessage, Stmt, TxOp, TxRules, TxState, run_sql};

pub type Repaint = Arc<dyn Fn() + Send + Sync>;

pub(crate) fn not_connected() -> DriverError {
    DriverError::new(ErrorKind::Connection, "not connected")
}

/// Chars of a statement's first line in its Messages entry.
const MESSAGE_SQL_CHARS: usize = 80;

/// What one connection overrides of the global settings the engine follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectionSettings {
    pub session_per_tab: Option<bool>,
    pub connect_timeout_s: Option<u32>,
    pub row_cap: Option<usize>,
}

/// Rows a `fetch_all` keeps before it truncates, until the settings say otherwise.
pub const DEFAULT_ROW_CAP: usize = 100_000;

pub struct Engine {
    drivers: DriverRegistry,
    pub(crate) shared: Arc<Shared>,
    /// The only strong sender of the runtime's commands (`Shared` holds a weak one);
    /// `None` only while dropping, and closing the channel stops the runtime.
    tx: Option<UnboundedSender<Command>>,
    thread: Option<JoinHandle<()>>,
}

pub(crate) enum Command {
    Connect {
        key: SessionKey,
        driver: Arc<dyn Driver>,
        cfg: ConnectionConfig,
        secrets: Secrets,
        io: Arc<SessionIo>,
        read_only: bool,
        tunnel: Arc<TunnelSlot>,
    },
    Browse {
        conn_id: String,
        path: NodePath,
        io: Arc<SessionIo>,
    },
    /// Runs a task on the runtime (object loads and DDL applies, `crate::objects`).
    Spawn(std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>),
    Execute {
        q: QueryId,
        ticket: u64,
        database: Option<String>,
        io: Arc<SessionIo>,
    },
    Fetch {
        q: QueryId,
        all: bool,
    },
    Cancel(Arc<dyn CancelHandle>),
    /// Closes the session's open stream (only if it belongs to `only`, when given).
    CloseStream {
        io: Arc<SessionIo>,
        only: Option<QueryId>,
    },
    LoadCatalog {
        conn_id: String,
        /// The database the catalog describes (`None` for the connection's own).
        database: Option<String>,
        need: CatalogNeed,
        io: Arc<SessionIo>,
        token: u64,
    },
    RunScript(ScriptRun),
    /// A transaction command or read-only switch, in the session's execute order.
    Tx {
        key: SessionKey,
        io: Arc<SessionIo>,
        ticket: u64,
        op: TxOp,
    },
    /// A changeset, in the session's execute order.
    Apply {
        job: JobControl,
        key: SessionKey,
        io: Arc<SessionIo>,
        ticket: u64,
        changeset: Changeset,
    },
    Describe {
        key: SessionKey,
        io: Arc<SessionIo>,
        table: TableRef,
    },
    /// Export, import, preview and copy jobs (see `transfer.rs`).
    Transfer(TransferRun),
    /// A native dump or restore, run on its own task so other commands keep flowing.
    Tool {
        job: JobControl,
        conn: String,
        database: String,
        path: PathBuf,
        work: crate::tools::ToolWork,
    },
}

impl Engine {
    /// An engine on an in-memory `sqlr.db`: nothing outlives it.
    pub fn new(drivers: DriverRegistry, repaint: Repaint) -> Self {
        let db = Db::open_in_memory().expect("open an in-memory database");
        Self::with_db(drivers, repaint, Arc::new(db))
    }

    /// Spawns a 2-worker tokio runtime on its own thread. The catalog cache and the
    /// snippets are restored from `db`, and its history trimmed, in the background; dropping
    /// the engine flushes pending writes to it.
    pub fn with_db(drivers: DriverRegistry, repaint: Repaint, db: Arc<Db>) -> Self {
        let (db_queue, db_jobs) = DbQueue::new();
        let (tx, mut rx) = unbounded_channel::<Command>();
        let operations = crate::operations::Operations::default();
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner::default()),
            catalog: CatalogCache::default(),
            library: Library::default(),
            tasks: crate::tasks::Tasks::default(),
            drivers_hub: crate::drivers::DriverHub::new(drivers.clone()),
            scripts: Scripts::default(),
            jobs: Jobs::with_operations(operations.clone()),
            operations,
            db: db_queue,
            commands: tx.downgrade(),
            repaint,
            per_tab: AtomicBool::new(true),
            connect_timeout_s: AtomicU64::new(15),
            row_cap: AtomicUsize::new(DEFAULT_ROW_CAP),
            conn_settings: Mutex::new(HashMap::new()),
            export_formats: Mutex::new(None),
            ssh: SshContext::default(),
            secret_store: Mutex::new(None),
            profiles: Mutex::new(ProfilesState::default()),
            kv: crate::kv::KvStates::default(),
            docs: crate::docs::DocStates::default(),
        });
        // The first job: trimming a large history is slow, so it never runs on the caller's
        // thread (the UI's, at startup).
        shared.db.push(|db| {
            if let Err(e) = db.trim_history() {
                tracing::warn!("could not trim the history: {e}");
            }
        });
        shared.load_snippets();
        shared.load_tasks();
        shared.load_recent();
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("sqlr-engine".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("sqlr-engine-worker")
                    .enable_all()
                    .build()
                    .expect("build the engine runtime");
                rt.block_on(async move {
                    let db_worker = tokio::spawn(run_db_worker(worker.clone(), db, db_jobs));
                    while let Some(cmd) = rx.recv().await {
                        tokio::spawn(worker.clone().handle(cmd));
                    }
                    // Writes already queued still reach the file.
                    worker.db.close();
                    let _ = tokio::time::timeout(Duration::from_secs(2), db_worker).await;
                });
                // Tasks still parked (e.g. a fetch the server never answers) are dropped.
                rt.shutdown_timeout(Duration::from_secs(1));
            })
            .expect("spawn the engine thread");
        Self {
            drivers,
            shared,
            tx: Some(tx),
            thread: Some(thread),
        }
    }

    pub fn drivers(&self) -> &DriverRegistry {
        &self.drivers
    }

    /// Every driver's manifest, sorted by name, with the engine's SSH fields added to those
    /// whose capabilities include `ssh`.
    pub fn manifests(&self) -> Vec<DriverManifest> {
        self.drivers
            .manifests()
            .into_iter()
            .map(ssh::with_ssh_fields)
            .collect()
    }

    /// The manifest of `driver`, with SSH fields like [`Engine::manifests`].
    pub fn manifest(&self, driver: &str) -> Option<DriverManifest> {
        self.drivers
            .get(driver)
            .map(|d| ssh::with_ssh_fields(d.manifest().clone()))
    }

    /// Seconds allowed for SSH setup plus the driver's connect (default 15).
    pub fn set_connect_timeout(&self, seconds: u32) {
        self.shared
            .connect_timeout_s
            .store(u64::from(seconds.max(1)), Ordering::Relaxed);
    }

    /// Where SSH host keys are recorded; without one they are remembered for the engine's
    /// lifetime only.
    pub fn set_known_hosts(&self, path: std::path::PathBuf) {
        *self.shared.ssh.known_hosts.lock() = Some(path);
    }

    /// The keychain secrets of saved connections are read from, on connect and off the UI
    /// thread; without one only the secrets given to [`Engine::connect`] are used.
    pub fn set_secret_store(&self, store: Arc<dyn sqlr_store::SecretStore>) {
        *self.shared.secret_store.lock() = Some(store);
    }

    /// Reads the connection profiles of `dir` in the background; see [`Engine::profiles`].
    pub fn load_profiles(&self, dir: std::path::PathBuf) {
        self.shared.load_profiles(dir);
    }

    /// Saves a profile (an override, for a built-in id) to `dir` in the background, then
    /// reloads the profiles. Failures become notices.
    pub fn save_profile(&self, dir: std::path::PathBuf, profile: ConnectionProfile) {
        self.shared.save_profile(dir, profile);
    }

    /// Deletes a user profile (or a built-in's override) from `dir` in the background, then
    /// reloads the profiles. Failures become notices.
    pub fn delete_profile(&self, dir: std::path::PathBuf, id: String) {
        self.shared.delete_profile(dir, id);
    }

    /// The profiles as last read; `None` until the first load finished.
    pub fn profiles(&self) -> Option<Arc<Vec<ConnectionProfile>>> {
        self.shared.profiles.lock().list.clone()
    }

    /// Bumps whenever [`Engine::profiles`] changes.
    pub fn profiles_revision(&self) -> u64 {
        self.shared.profiles.lock().revision
    }

    /// Starts the connection's explorer session, replacing any existing one, and keeps
    /// `cfg`, `secrets` and `profile` to open its tab sessions later. A changed `cfg` or
    /// `secrets` closes the tab sessions opened with the old ones.
    /// Status: `Connecting`, then `Connected` or `Failed`.
    pub fn connect(&self, cfg: ConnectionConfig, secrets: Secrets, profile: ConnectionProfile) {
        let key = SessionKey::meta(&cfg.id);
        let driver = self.drivers.get(&cfg.driver);
        self.shared.kv.forget(&cfg.id);
        {
            let mut inner = self.shared.inner.lock();
            inner.explorer.remove(&cfg.id);
            inner.tables.retain(|(conn, _), _| *conn != cfg.id);
            match driver {
                None => {
                    let io = SessionIo::new();
                    io.finish_connect();
                    let failed = SessionStatus::Failed(DriverError::new(
                        ErrorKind::Unsupported,
                        format!("unknown driver \"{}\"", cfg.driver),
                    ));
                    let tx = TxState::new(profile.commit_mode, profile.read_only, now_ms());
                    inner.conns.remove(&cfg.id);
                    if let Some(old) = inner.sessions.insert(key, Session::new(failed, io, tx)) {
                        self.shared.retire(old);
                    }
                }
                Some(driver) => {
                    let changed = inner
                        .conns
                        .get(&cfg.id)
                        .is_none_or(|c| c.cfg != cfg || c.secrets != secrets);
                    let new_profile = inner
                        .conns
                        .get(&cfg.id)
                        .is_none_or(|c| c.profile != profile);
                    if changed || new_profile {
                        // Session settings the user picked follow the new config or profile.
                        inner.prefs.retain(|k, _| k.conn != cfg.id);
                    }
                    if changed {
                        let tabs: Vec<SessionKey> = inner
                            .sessions
                            .keys()
                            .filter(|k| k.conn == cfg.id && k.slot != Slot::Meta)
                            .cloned()
                            .collect();
                        for k in tabs {
                            if let Some(old) = inner.sessions.remove(&k) {
                                self.shared.retire(old);
                            }
                        }
                    }
                    let tunnel = match inner.conns.remove(&cfg.id) {
                        Some(old) if !changed => old.tunnel,
                        _ => Arc::default(),
                    };
                    inner.conns.insert(
                        cfg.id.clone(),
                        ConnEntry {
                            driver,
                            cfg,
                            secrets,
                            profile,
                            tunnel,
                            resolved: None,
                        },
                    );
                    self.shared.open_session(&mut inner, &key);
                }
            }
        }
        self.shared.repaint();
    }

    /// Closes every session of the connection (the explorer's ends `Disconnected`), forgets
    /// what `connect` kept, and clears its explorer cache.
    pub fn disconnect(&self, conn_id: &str) {
        self.shared.kv.forget(conn_id);
        self.shared.end_docs(conn_id);
        {
            let mut inner = self.shared.inner.lock();
            let key = SessionKey::meta(conn_id);
            let Some(meta) = inner.sessions.get(&key) else {
                return;
            };
            let tx = TxState::new(meta.tx.mode, meta.tx.read_only, now_ms());
            let disconnected = Session::new(SessionStatus::Disconnected, meta.io.clone(), tx);
            let keys: Vec<SessionKey> = inner
                .sessions
                .keys()
                .filter(|k| k.conn == conn_id)
                .cloned()
                .collect();
            for k in keys {
                if let Some(old) = inner.sessions.remove(&k) {
                    self.shared.retire(old);
                }
            }
            inner.sessions.insert(key, disconnected);
            inner.conns.remove(conn_id);
            // The next connect starts from the profile again.
            inner.prefs.retain(|k, _| k.conn != conn_id);
            inner.explorer.remove(conn_id);
            inner.tables.retain(|(conn, _), _| conn != conn_id);
        }
        self.shared.repaint();
    }

    /// The status of the connection's explorer session.
    pub fn session_status(&self, conn_id: &str) -> Option<SessionStatus> {
        let inner = self.shared.inner.lock();
        inner
            .sessions
            .get(&SessionKey::meta(conn_id))
            .map(|s| s.status.clone())
    }

    /// How long the explorer session's connect took and the SSH host key it met, once
    /// connected.
    pub fn connect_report(&self, conn_id: &str) -> Option<ConnectReport> {
        let inner = self.shared.inner.lock();
        inner.reports.get(conn_id).cloned()
    }

    /// The database the session's own connection is attached to, once connected and when the
    /// driver names one.
    pub fn session_database(&self, conn_id: &str) -> Option<String> {
        let inner = self.shared.inner.lock();
        let conn = inner
            .sessions
            .get(&SessionKey::meta(conn_id))?
            .conn
            .clone()?;
        conn.database_name().map(str::to_string)
    }

    /// The session a tab runs on: its own (`Slot::Tab`) while sessions per tab are on, or
    /// while it still has one opened before they were turned off; else the connection's
    /// explorer session.
    pub fn session(&self, conn: &str, tab: Option<u64>) -> SessionKey {
        let per_tab = self.shared.session_per_tab_for(conn);
        let own = |t: u64| SessionKey {
            conn: conn.to_string(),
            slot: Slot::Tab(t),
        };
        match tab {
            Some(t) if per_tab || self.shared.inner.lock().sessions.contains_key(&own(t)) => own(t),
            _ => SessionKey::meta(conn),
        }
    }

    /// Whether each editor tab gets its own session (default on); affects later
    /// [`Engine::session`] calls only, and a tab that has its own session keeps it until
    /// the session is closed.
    pub fn set_session_per_tab(&self, on: bool) {
        self.shared.per_tab.store(on, Ordering::Relaxed);
    }

    /// Closes a tab's session and its connection (an open transaction is rolled back by
    /// the server). The explorer session is left alone.
    pub fn close_session(&self, key: &SessionKey) {
        if key.slot == Slot::Meta {
            return;
        }
        {
            let mut inner = self.shared.inner.lock();
            inner.prefs.remove(key);
            if let Some(old) = inner.sessions.remove(key) {
                self.shared.retire(old);
            }
        }
        self.shared.repaint();
    }

    /// The engine session key for an MCP agent (`agent_id` is the MCP session id) on
    /// `conn`.
    pub fn agent_session(&self, conn: &str, agent_id: &str) -> SessionKey {
        SessionKey::agent(conn, agent_id)
    }

    /// Prepares an agent session: `read_only` forces the server-side read-only flag (a
    /// `Read` exposure), and the profile still supplies the commit mode. The session opens
    /// lazily on its first query.
    pub fn prepare_agent_session(&self, key: &SessionKey, read_only: bool) {
        if !matches!(key.slot, Slot::Agent(_)) {
            return;
        }
        let mut inner = self.shared.inner.lock();
        let Some(entry) = inner.conns.get(&key.conn) else {
            return;
        };
        let mode = entry.profile.commit_mode;
        let profile_read_only = entry.profile.read_only;
        inner
            .prefs
            .insert(key.clone(), (mode, read_only || profile_read_only));
    }

    /// Closes every agent session of one MCP session: pending queries are cancelled, their
    /// results closed and the connections retired (open transactions roll back when the
    /// server connection goes away).
    pub fn close_agent_sessions(&self, agent_id: &str) {
        let mut results = Vec::new();
        {
            let mut inner = self.shared.inner.lock();
            let keys: Vec<SessionKey> = inner
                .sessions
                .keys()
                .filter(|k| matches!(&k.slot, Slot::Agent(a) if a == agent_id))
                .cloned()
                .collect();
            results.extend(
                inner
                    .results
                    .iter()
                    .filter(|(_, e)| matches!(&e.key.slot, Slot::Agent(a) if a == agent_id))
                    .map(|(q, _)| *q),
            );
            for key in keys {
                inner.prefs.remove(&key);
                if let Some(old) = inner.sessions.remove(&key) {
                    self.shared.retire(old);
                }
            }
        }
        for q in results {
            self.shared.close_query(q);
        }
        self.shared.close_agent_docs(agent_id);
        self.shared.repaint();
    }

    /// Closes every agent session on one connection (exposure became `None`).
    pub fn close_agent_sessions_for_connection(&self, conn: &str) {
        let mut results = Vec::new();
        {
            let mut inner = self.shared.inner.lock();
            let keys: Vec<SessionKey> = inner
                .sessions
                .keys()
                .filter(|k| k.conn == conn && matches!(k.slot, Slot::Agent(_)))
                .cloned()
                .collect();
            results.extend(
                inner
                    .results
                    .iter()
                    .filter(|(_, e)| e.key.conn == conn && matches!(e.key.slot, Slot::Agent(_)))
                    .map(|(q, _)| *q),
            );
            for key in keys {
                inner.prefs.remove(&key);
                if let Some(old) = inner.sessions.remove(&key) {
                    self.shared.retire(old);
                }
            }
        }
        for q in results {
            self.shared.close_query(q);
        }
        self.shared.repaint();
    }

    /// Whether the connection's metadata session was opened and is live (the explorer
    /// shows the connection).
    pub fn is_connected(&self, conn_id: &str) -> bool {
        self.shared.live_io(conn_id).is_some()
    }

    /// Loads the children of `path` into the explorer cache; a no-op if they are already
    /// loading or loaded. Waits for a session that is still connecting.
    pub fn browse(&self, conn_id: &str, path: NodePath) {
        {
            let mut inner = self.shared.inner.lock();
            let io = inner
                .sessions
                .get(&SessionKey::meta(conn_id))
                .filter(|s| s.is_live())
                .map(|s| s.io.clone());
            let cache = inner.explorer.entry(conn_id.to_string()).or_default();
            if matches!(
                cache.get(&path),
                Some(LoadState::Loading | LoadState::Ready(_))
            ) {
                return;
            }
            match io {
                None => {
                    cache.insert(path, LoadState::Failed(not_connected()));
                }
                Some(io) => {
                    cache.insert(path.clone(), LoadState::Loading);
                    self.send(Command::Browse {
                        conn_id: conn_id.to_string(),
                        path,
                        io,
                    });
                }
            }
        }
        self.shared.repaint();
    }

    /// Drops the cached subtree at `path`, then browses it again. Refreshing the root, or
    /// the node that holds the connection's schemas, also invalidates its catalog (§7.8).
    pub fn refresh(&self, conn_id: &str, path: NodePath) {
        let schema_path = {
            let inner = self.shared.inner.lock();
            let conn = inner
                .sessions
                .get(&SessionKey::meta(conn_id))
                .and_then(|s| s.conn.clone());
            conn.map(|c| c.schema_path()).unwrap_or_default()
        };
        if schema_path.segments().starts_with(path.segments()) {
            self.invalidate_catalog(conn_id);
        }
        {
            let mut inner = self.shared.inner.lock();
            if let Some(cache) = inner.explorer.get_mut(conn_id) {
                cache.retain(|p, _| !p.segments().starts_with(path.segments()));
            }
        }
        self.browse(conn_id, path);
    }

    pub fn children(&self, conn_id: &str, path: &NodePath) -> Option<LoadState<Vec<Node>>> {
        let inner = self.shared.inner.lock();
        inner.explorer.get(conn_id)?.get(path).cloned()
    }

    /// SQL that opens `node`'s data, in the connected session's dialect.
    pub fn open_data_sql(
        &self,
        conn_id: &str,
        node: &Node,
        limit: Option<usize>,
    ) -> Option<String> {
        self.open_data_target(conn_id, node, limit)
            .map(|(_, sql)| sql)
    }

    /// The database a node's data must be read from (when it is not the connection's own)
    /// and the SQL that opens it.
    pub fn open_data_target(
        &self,
        conn_id: &str,
        node: &Node,
        limit: Option<usize>,
    ) -> Option<(Option<String>, String)> {
        let conn = {
            let inner = self.shared.inner.lock();
            inner
                .sessions
                .get(&SessionKey::meta(conn_id))?
                .conn
                .clone()?
        };
        let database = conn.database_for_path(&node.path);
        let sql = conn.relational()?.dialect().select_all(node, limit)?;
        Some((database, sql))
    }

    /// Runs `sql` on the explorer session; see [`Engine::execute_in`].
    pub fn execute(&self, conn_id: &str, sql: String, page_size: usize) -> QueryId {
        self.execute_in(&SessionKey::meta(conn_id), None, sql, page_size)
    }

    /// Like [`Engine::execute`], on the server database `database` when given.
    pub fn execute_on(
        &self,
        conn_id: &str,
        database: Option<&str>,
        sql: String,
        page_size: usize,
    ) -> QueryId {
        self.execute_in(&SessionKey::meta(conn_id), database, sql, page_size)
    }

    /// Runs `sql` on the session `key` (on the server database `database` when given) and
    /// fetches its first page. A tab session opens on its first run, and any session whose
    /// connection was lost reconnects. Earlier queries on the session stop fetching and end
    /// `Ready { more: false }` (`cut_short` if rows may remain); executes on one session
    /// start in issue order. The session's commit mode and read-only flag apply (§7.9).
    pub fn execute_in(
        &self,
        key: &SessionKey,
        database: Option<&str>,
        sql: String,
        page_size: usize,
    ) -> QueryId {
        let database = database.map(str::to_string);
        let (q, _) = self
            .shared
            .start_query(key, database, sql, page_size, false);
        q
    }

    /// Like [`Engine::execute_in`], but a read (`StatementClass::Read`) opens no transaction
    /// in Manual mode: an object editor's Data page, whose edits open one when applied. A
    /// read inside an open transaction still runs in it; any other statement follows the
    /// commit mode.
    pub fn read_in(
        &self,
        key: &SessionKey,
        database: Option<&str>,
        sql: String,
        page_size: usize,
    ) -> QueryId {
        let database = database.map(str::to_string);
        let (q, _) = self.shared.start_query(key, database, sql, page_size, true);
        q
    }

    /// Runs `statements` on the explorer session; see [`Engine::execute_script_in`].
    pub fn execute_script(
        &self,
        conn_id: &str,
        statements: Vec<String>,
        page_size: usize,
        on_error: ScriptOnError,
    ) -> ScriptId {
        self.execute_script_in(
            &SessionKey::meta(conn_id),
            None,
            statements,
            page_size,
            on_error,
        )
    }

    /// Like [`Engine::execute_script`], on the server database `database` when given.
    pub fn execute_script_on(
        &self,
        conn_id: &str,
        database: Option<&str>,
        statements: Vec<String>,
        page_size: usize,
        on_error: ScriptOnError,
    ) -> ScriptId {
        self.execute_script_in(
            &SessionKey::meta(conn_id),
            database,
            statements,
            page_size,
            on_error,
        )
    }

    /// Runs `statements` one after another on the session `key`, each as its own query
    /// (with its own result and history entry), stopping at the first failure with `Stop`.
    /// Each statement waits for the previous one's first page or outcome, then takes the
    /// session's stream over, so an earlier rows result with rows left ends `cut_short`.
    pub fn execute_script_in(
        &self,
        key: &SessionKey,
        database: Option<&str>,
        statements: Vec<String>,
        page_size: usize,
        on_error: ScriptOnError,
    ) -> ScriptId {
        let (id, run) = self.shared.start_script(
            key,
            database.map(str::to_string),
            statements,
            page_size,
            on_error,
        );
        self.send(Command::RunScript(run));
        self.shared.repaint();
        id
    }

    /// The script's progress. Do not hold its guard while calling other `Engine` methods.
    pub fn script(&self, id: ScriptId) -> Option<Arc<RwLock<ScriptState>>> {
        self.shared.scripts.state(id)
    }

    /// Cancels the running statement; the script runs no more.
    pub fn cancel_script(&self, id: ScriptId) {
        self.shared.cancel_script(id);
    }

    /// Closes every result of the script, then forgets it.
    pub fn close_script(&self, id: ScriptId) {
        self.shared.close_script(id);
    }

    /// The session's transaction state: its own once opened, else the state it will open
    /// with. `None` for a connection that is not connected.
    pub fn transaction(&self, key: &SessionKey) -> Option<TxState> {
        let inner = self.shared.inner.lock();
        let timeout = inner
            .conns
            .get(&key.conn)
            .and_then(|c| c.profile.idle_tx_timeout_min);
        match inner.sessions.get(key) {
            Some(s) => {
                let mut tx = s.tx.clone();
                tx.idle_timeout = tx.is_open()
                    && s.active.is_none()
                    && timeout.is_some_and(|min| {
                        now_ms() - tx.last_activity_ms >= i64::from(min) * 60_000
                    });
                Some(tx)
            }
            None => inner.initial_tx(key),
        }
    }

    /// Every session with an open (or failed) transaction, sorted by connection then slot.
    pub fn open_transactions(&self) -> Vec<(SessionKey, TxState)> {
        let mut open: Vec<(SessionKey, TxState)> = {
            let inner = self.shared.inner.lock();
            inner
                .sessions
                .iter()
                .filter(|(_, s)| s.tx.is_open())
                .map(|(k, _)| k.clone())
                .collect::<Vec<_>>()
        }
        .into_iter()
        .filter_map(|k| Some((k.clone(), self.transaction(&k)?)))
        .collect();
        open.sort_by_key(|(k, _)| {
            let slot = match &k.slot {
                Slot::Meta => 0,
                Slot::Tab(t) => t + 1,
                Slot::Agent(_) => 1,
            };
            (k.conn.clone(), slot)
        });
        open
    }

    /// Sets the session's commit mode. Switching to `Auto` while a transaction is open is
    /// refused (the UI asks to commit or roll back first).
    pub fn set_commit_mode(&self, key: &SessionKey, mode: CommitMode) {
        {
            let mut inner = self.shared.inner.lock();
            let Some(mut tx) = inner
                .sessions
                .get(key)
                .map(|s| s.tx.clone())
                .or_else(|| inner.initial_tx(key))
            else {
                return;
            };
            if mode == CommitMode::Auto && tx.is_open() {
                return;
            }
            tx.mode = mode;
            if let Some(s) = inner.sessions.get_mut(key) {
                s.tx.mode = mode;
            }
            inner.prefs.insert(key.clone(), (tx.mode, tx.read_only));
        }
        self.shared.repaint();
    }

    /// Turns the session's read-only mode on or off: the dialect's `read_only_sql` runs in
    /// the session's order (and again after a reconnect), and statements that modify are
    /// refused before they reach the driver. On a live session the flag changes once the
    /// server accepted the switch; a refusal keeps it and becomes a notice.
    pub fn set_read_only(&self, key: &SessionKey, on: bool) {
        {
            let mut inner = self.shared.inner.lock();
            let Some(tx) = inner
                .sessions
                .get(key)
                .map(|s| s.tx.clone())
                .or_else(|| inner.initial_tx(key))
            else {
                return;
            };
            match inner.sessions.get_mut(key) {
                Some(s) if s.is_live() => {
                    let io = s.io.clone();
                    self.send(Command::Tx {
                        key: key.clone(),
                        ticket: io.issue_ticket(),
                        io,
                        op: TxOp::SetReadOnly(on),
                    });
                }
                // Applied when the session (re)connects.
                Some(s) => {
                    s.tx.read_only = on;
                    inner.prefs.insert(key.clone(), (tx.mode, on));
                }
                None => {
                    inner.prefs.insert(key.clone(), (tx.mode, on));
                }
            }
        }
        self.shared.repaint();
    }

    /// Commits the session's open transaction.
    pub fn commit(&self, key: &SessionKey) {
        self.tx_op(key, TxOp::Commit);
    }

    /// Rolls the session's open transaction back, interrupting a running query first.
    pub fn rollback(&self, key: &SessionKey) {
        let active = {
            let inner = self.shared.inner.lock();
            inner.sessions.get(key).and_then(|s| s.active)
        };
        if let Some(q) = active {
            self.shared.cancel_query(q);
        }
        self.tx_op(key, TxOp::Rollback);
    }

    /// Sets a savepoint in the open transaction.
    pub fn savepoint(&self, key: &SessionKey, name: String) {
        self.tx_op(key, TxOp::Savepoint(name));
    }

    /// Rolls the open transaction back to `name`; later savepoints are forgotten.
    pub fn rollback_to(&self, key: &SessionKey, name: String) {
        self.tx_op(key, TxOp::RollbackTo(name));
    }

    /// Fetches one more page; ignored unless the result is `Ready { more: true }`.
    pub fn fetch_more(&self, q: QueryId) {
        self.start_fetch(q, false);
    }

    pub fn set_page_size(&self, q: QueryId, page_size: usize) {
        if let Some(result) = self.result(q) {
            result.write().page_size = page_size.max(1);
            self.shared.repaint();
        }
    }

    /// Caps the rows a `fetch_all` keeps: at the cap the stream is closed and the result
    /// ends `Ready { more: false }` with a warning note. Rows fetched by `fetch_more` count.
    /// `0` means no cap.
    pub fn set_row_cap(&self, cap: usize) {
        let cap = if cap == 0 { usize::MAX } else { cap };
        self.shared.row_cap.store(cap, Ordering::Relaxed);
    }

    /// What one connection overrides of the global session, timeout and row-cap settings;
    /// the default (nothing overridden) clears it. A `row_cap` of `0` means no cap.
    pub fn set_connection_settings(&self, conn_id: &str, settings: ConnectionSettings) {
        let mut all = self.shared.conn_settings.lock();
        if settings == ConnectionSettings::default() {
            all.remove(conn_id);
        } else {
            all.insert(conn_id.to_string(), settings);
        }
    }

    /// The display formats text exports (CSV, TSV, Markdown) write with, or `None` for the
    /// raw values.
    pub fn set_export_formats(&self, formats: Option<sqlr_core::DataFormats>) {
        *self.shared.export_formats.lock() = formats;
    }

    /// Deletes the connection's history entries, or every connection's with `None`.
    pub fn clear_history(&self, conn_id: Option<&str>) {
        self.shared.clear_history(conn_id.map(str::to_string));
    }

    /// Fetches page after page until the stream is exhausted.
    pub fn fetch_all(&self, q: QueryId) {
        self.start_fetch(q, true);
    }

    /// Interrupts a running query; a no-op if it is not running.
    pub fn cancel(&self, q: QueryId) {
        self.shared.cancel_query(q);
    }

    /// Forgets the result, interrupting it if running and closing its stream.
    pub fn close_result(&self, q: QueryId) {
        self.shared.close_query(q);
    }

    /// The shared result, updated by the runtime as pages arrive. Do not hold its guard
    /// while calling other `Engine` methods: some of them lock the result too.
    pub fn result(&self, q: QueryId) -> Option<Arc<RwLock<QueryResult>>> {
        self.shared.result(q)
    }

    /// A snapshot of the connection's own catalog; cheap to take every frame.
    pub fn catalog(&self, conn_id: &str) -> Option<Arc<ConnCatalog>> {
        self.catalog_in(conn_id, None)
    }

    /// A snapshot of the catalog of `database` on `conn_id` (`None` for the connection's
    /// own database); cheap to take every frame.
    pub fn catalog_in(&self, conn_id: &str, database: Option<&str>) -> Option<Arc<ConnCatalog>> {
        self.shared.catalog.snapshot(conn_id, database)
    }

    /// Every catalog the engine holds, sorted by connection id.
    pub fn all_catalogs(&self) -> Vec<(String, Arc<ConnCatalog>)> {
        self.shared.catalog.all()
    }

    /// Bumps on every catalog change.
    pub fn catalog_revision(&self) -> u64 {
        self.shared.catalog.revision()
    }

    /// Loads `need` into the catalog in the background. Ignored without a live session;
    /// a no-op while it is loading, or once it is loaded and fresh. Waits for a session
    /// that is still connecting.
    pub fn load_catalog(&self, conn_id: &str, need: CatalogNeed) {
        self.load_catalog_in(conn_id, None, need);
    }

    /// Like [`Engine::load_catalog`], for `database` on the same connection (`None` for the
    /// connection's own database). A catalog of another database opens a connection to it
    /// when needed.
    pub fn load_catalog_in(&self, conn_id: &str, database: Option<&str>, need: CatalogNeed) {
        let Some(io) = self.shared.live_io(conn_id) else {
            return;
        };
        if let Some(token) = self
            .shared
            .catalog
            .begin(conn_id, database, &need, &io, false)
        {
            self.send(Command::LoadCatalog {
                conn_id: conn_id.to_string(),
                database: database.map(str::to_string),
                need,
                io,
                token,
            });
            self.shared.repaint();
        }
    }

    /// Forgets the connection's catalog, in memory and in `sqlr.db`.
    pub fn invalidate_catalog(&self, conn_id: &str) {
        self.shared.invalidate_catalog(conn_id);
    }

    /// Replaces the channel's query; its results land in [`Engine::history`].
    pub fn search_history(&self, channel: HistoryChannel, q: HistoryQuery) {
        self.shared.search_history(channel, q);
    }

    /// The channel's query and results; `None` until it is first searched. The results
    /// follow new history entries.
    pub fn history(&self, channel: HistoryChannel) -> Option<HistoryView> {
        self.shared.history(channel)
    }

    /// Every snippet, sorted by `(folder, name)`; empty until loaded at startup.
    pub fn snippets(&self) -> Arc<Vec<Snippet>> {
        self.shared.snippets()
    }

    /// Inserts or replaces (by id) a snippet: in memory at once, in `sqlr.db` behind.
    pub fn save_snippet(&self, s: Snippet) {
        self.shared.save_snippet(s);
    }

    pub fn delete_snippet(&self, id: &str) {
        self.shared.delete_snippet(id);
    }

    /// Saved connections by when they last connected, most recent first: loaded from
    /// `sqlr.db` at startup and updated on every successful connect (not for transient
    /// sessions, whose ids hold a `:`).
    pub fn recent(&self) -> Arc<Vec<(String, i64)>> {
        self.shared.recent()
    }

    /// Forgets a deleted connection's recent use.
    pub fn forget_recent(&self, conn_id: &str) {
        self.shared.forget_recent(conn_id);
    }

    /// How many children of `path` are loaded, without copying them; `None` until they are.
    pub fn child_count(&self, conn_id: &str, path: &NodePath) -> Option<usize> {
        let inner = self.shared.inner.lock();
        match inner.explorer.get(conn_id)?.get(path)? {
            LoadState::Ready(nodes) => Some(nodes.len()),
            _ => None,
        }
    }

    /// Background failures (persistence, commits and other transaction commands) since the
    /// last call, oldest first.
    pub fn take_notices(&self) -> Vec<String> {
        self.shared.take_notices()
    }

    /// Describes `table` (columns and keys) on the session `key` in the background; the
    /// result lands in [`Engine::table_info`]. A no-op while it is loading.
    pub fn describe(&self, key: &SessionKey, table: TableRef) {
        {
            let mut inner = self.shared.inner.lock();
            let entry = (key.conn.clone(), table.clone());
            if inner.tables.get(&entry) == Some(&LoadState::Loading) {
                return;
            }
            let io = inner
                .sessions
                .get(key)
                .filter(|s| s.is_live())
                .map(|s| s.io.clone());
            match io {
                None => {
                    inner
                        .tables
                        .insert(entry, LoadState::Failed(not_connected()));
                }
                Some(io) => {
                    inner.tables.insert(entry, LoadState::Loading);
                    self.send(Command::Describe {
                        key: key.clone(),
                        io,
                        table,
                    });
                }
            }
        }
        self.shared.repaint();
    }

    /// The description of `table` on connection `conn`, once [`Engine::describe`] ran.
    pub fn table_info(&self, conn: &str, table: &TableRef) -> Option<LoadState<TableInfo>> {
        let inner = self.shared.inner.lock();
        inner
            .tables
            .get(&(conn.to_string(), table.clone()))
            .cloned()
    }

    /// Applies `changeset` on the session `key`, in the session's order: in its own
    /// transaction under auto-commit, else inside the session's transaction (opened when
    /// none is), behind a savepoint. Every `UPDATE` and `DELETE` must affect exactly one
    /// row, or the whole changeset is undone and the row named
    /// ([`ApplyReport::failed`](crate::ApplyReport)). Refused on a read-only session.
    pub fn apply_changeset(&self, key: &SessionKey, changeset: Changeset) -> JobId {
        let job = self.shared.jobs.create(JobKind::Apply);
        let id = job.id;
        let io = {
            let inner = self.shared.inner.lock();
            inner
                .sessions
                .get(key)
                .filter(|s| s.is_live())
                .map(|s| s.io.clone())
        };
        match io {
            None => self
                .shared
                .jobs
                .finish(id, JobOutcome::Failed(not_connected())),
            Some(io) => self.send(Command::Apply {
                job,
                key: key.clone(),
                ticket: io.issue_ticket(),
                io,
                changeset,
            }),
        }
        self.shared.repaint();
        id
    }

    /// Writes `source` to `path` in `format` as a job: a re-run streams every row in chunks
    /// on its session (in the session's order); the file appears only once complete.
    pub fn export(&self, source: ExportSource, format: TextFormat, path: PathBuf) -> JobId {
        let job = self.shared.jobs.create(JobKind::Export);
        let id = job.id;
        let work = match source {
            ExportSource::Loaded(q) => Work::ExportLoaded { q, format, path },
            ExportSource::Rerun { key, database, sql } => match self.live_io(&key) {
                None => {
                    self.shared
                        .jobs
                        .finish(id, JobOutcome::Failed(not_connected()));
                    return id;
                }
                Some(io) => Work::ExportRerun {
                    key,
                    ticket: io.issue_ticket(),
                    io,
                    database,
                    sql,
                    format,
                    path,
                },
            },
        };
        self.send(Command::Transfer(TransferRun { job, work }));
        self.shared.repaint();
        id
    }

    /// Dumps the whole `database` of `conn` with its native tool to `path` (a file, or a
    /// directory for PostgreSQL's directory format), as a [`JobKind::Dump`] job.
    pub fn run_dump(
        &self,
        conn: &str,
        database: &str,
        options: DumpOptions,
        path: PathBuf,
    ) -> JobId {
        let job = self.shared.jobs.create(JobKind::Dump);
        let id = job.id;
        self.send(Command::Tool {
            job,
            conn: conn.to_string(),
            database: database.to_string(),
            path,
            work: crate::tools::ToolWork::Dump(options),
        });
        self.shared.repaint();
        id
    }

    /// Restores the archive at `path` into `database` on `conn` with its native tool, as a
    /// [`JobKind::Restore`] job.
    pub fn run_restore(
        &self,
        conn: &str,
        database: &str,
        options: RestoreOptions,
        path: PathBuf,
    ) -> JobId {
        let job = self.shared.jobs.create(JobKind::Restore);
        let id = job.id;
        self.send(Command::Tool {
            job,
            conn: conn.to_string(),
            database: database.to_string(),
            path,
            work: crate::tools::ToolWork::Restore(options),
        });
        self.shared.repaint();
        id
    }

    /// Inserts the rows of the file at `path` into `target` as a job, on the session `key`
    /// in its order: `mapping` pairs each source column with a target column (`None` skips
    /// it); text is converted to the target column's type. One transaction (or one per
    /// batch); refused on a read-only session or with a transaction open.
    pub fn import(
        &self,
        key: &SessionKey,
        path: PathBuf,
        format: TextFormat,
        target: TableRef,
        mapping: Vec<(usize, Option<String>)>,
        options: ImportOptions,
    ) -> JobId {
        let job = self.shared.jobs.create(JobKind::Import);
        let id = job.id;
        match self.live_io(key) {
            None => self
                .shared
                .jobs
                .finish(id, JobOutcome::Failed(not_connected())),
            Some(io) => self.send(Command::Transfer(TransferRun {
                job,
                work: Work::Import {
                    key: key.clone(),
                    ticket: io.issue_ticket(),
                    io,
                    path,
                    format,
                    target,
                    mapping: mapping.into_iter().map(Into::into).collect(),
                    options,
                },
            })),
        }
        self.shared.repaint();
        id
    }

    /// Reads the column names and the first 100 rows of a file as a job
    /// ([`JobOutcome::Preview`]).
    pub fn preview_file(&self, path: PathBuf, format: TextFormat) -> JobId {
        let job = self.shared.jobs.create(JobKind::Preview);
        let id = job.id;
        self.send(Command::Transfer(TransferRun {
            job,
            work: Work::Preview { path, format },
        }));
        id
    }

    /// Encodes the cells of `rows` × `cols` of a result (in the given orders) as a job
    /// ([`JobOutcome::Copied`]), for clipboard text too large to build on the UI thread.
    pub fn copy_text(
        &self,
        q: QueryId,
        rows: Vec<usize>,
        cols: Vec<usize>,
        format: TextFormat,
    ) -> JobId {
        let job = self.shared.jobs.create(JobKind::Clipboard);
        let id = job.id;
        self.send(Command::Transfer(TransferRun {
            job,
            work: Work::Copy {
                q,
                rows,
                cols,
                format,
            },
        }));
        id
    }

    /// Writes `text` to `path` as a job (an export of text the UI built, such as rejected
    /// import rows); ends `Exported` with its line count.
    pub fn save_text(&self, path: PathBuf, text: String) -> JobId {
        let job = self.shared.jobs.create(JobKind::Export);
        let id = job.id;
        self.send(Command::Transfer(TransferRun {
            job,
            work: Work::SaveText { path, text },
        }));
        id
    }

    /// The I/O handle of a live session.
    fn live_io(&self, key: &SessionKey) -> Option<Arc<SessionIo>> {
        let inner = self.shared.inner.lock();
        inner
            .sessions
            .get(key)
            .filter(|s| s.is_live())
            .map(|s| s.io.clone())
    }

    /// The job's progress and outcome; `None` for an unknown or forgotten job.
    pub fn job(&self, id: JobId) -> Option<JobState> {
        self.shared.jobs.state(id)
    }

    /// Stops a running job: an export leaves no file, an import rolls back what it has
    /// not committed. A no-op once the job ended.
    pub fn cancel_job(&self, id: JobId) {
        if let Some(handle) = self.shared.jobs.cancel(id) {
            self.send(Command::Cancel(handle));
        }
        self.shared.repaint();
    }

    /// Forgets a job's state (its outcome may hold preview rows or clipboard text).
    pub fn forget_job(&self, id: JobId) {
        self.shared.jobs.forget(id);
    }

    fn start_fetch(&self, q: QueryId, all: bool) {
        let Some(result) = self.result(q) else {
            return;
        };
        {
            let mut r = result.write();
            if r.status != (QueryStatus::Ready { more: true }) {
                return;
            }
            r.status = QueryStatus::Running;
        }
        self.send(Command::Fetch { q, all });
        self.shared.repaint();
    }

    /// Queues `op` behind the session's earlier executes; ignored without a live session.
    fn tx_op(&self, key: &SessionKey, op: TxOp) {
        let io = {
            let inner = self.shared.inner.lock();
            inner
                .sessions
                .get(key)
                .filter(|s| s.is_live())
                .map(|s| s.io.clone())
        };
        if let Some(io) = io {
            self.send(Command::Tx {
                key: key.clone(),
                ticket: io.issue_ticket(),
                io,
                op,
            });
        }
    }

    fn send(&self, cmd: Command) {
        self.shared.send(cmd);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct ResultEntry {
    result: Arc<RwLock<QueryResult>>,
    key: SessionKey,
    /// The session the query runs on; `None` if it failed before reaching one.
    io: Option<Arc<SessionIo>>,
    started: Instant,
    /// Wall-clock time of the `execute`, in Unix milliseconds.
    started_at_ms: i64,
    cancel_requested: bool,
    /// A later query was issued on the same session.
    superseded: bool,
    /// The query's history entry was recorded (when its status first left `Running`).
    recorded: bool,
    /// Signalled (once, with a stored permit) when the status first leaves `Running`, or
    /// when the result is closed before that.
    settled: Arc<Notify>,
    /// A read that opens no Manual transaction ([`Engine::read_in`]).
    outside_tx: bool,
}

/// What a native tool needs from a connection, cloned out of [`Inner`].
pub(crate) struct ToolConn {
    pub cfg: ConnectionConfig,
    pub manifest: DriverManifest,
    pub given: Secrets,
    pub resolved: Option<Secrets>,
    pub tunnel: Arc<TunnelSlot>,
}

/// What `connect` keeps to open (and reopen) the connection's sessions.
pub(crate) struct ConnEntry {
    driver: Arc<dyn Driver>,
    cfg: ConnectionConfig,
    secrets: Secrets,
    profile: ConnectionProfile,
    /// The connection's one SSH tunnel, shared by its sessions.
    tunnel: Arc<TunnelSlot>,
    /// `secrets` completed from the keychain on the first connect.
    resolved: Option<Secrets>,
}

#[derive(Default)]
pub(crate) struct Inner {
    pub(crate) sessions: HashMap<SessionKey, Session>,
    conns: HashMap<String, ConnEntry>,
    /// Commit mode and read-only flag the user set per session; they outlive reconnects.
    pub(crate) prefs: HashMap<SessionKey, (CommitMode, bool)>,
    /// The last connect report of each connection's explorer session.
    reports: HashMap<String, ConnectReport>,
    pub(crate) explorer: HashMap<String, HashMap<NodePath, LoadState<Vec<Node>>>>,
    /// Table descriptions by connection id and table.
    pub(crate) tables: HashMap<(String, TableRef), LoadState<TableInfo>>,
    results: HashMap<QueryId, ResultEntry>,
    next_query: u64,
}

impl Inner {
    /// The session for `key`, if it is still the one `io` belongs to.
    pub(crate) fn session(
        &mut self,
        key: &SessionKey,
        io: &Arc<SessionIo>,
    ) -> Option<&mut Session> {
        self.sessions
            .get_mut(key)
            .filter(|s| Arc::ptr_eq(&s.io, io))
    }

    /// The driver id of a connection, whether or not it is connected.
    pub(crate) fn driver_of(&self, conn: &str) -> Option<String> {
        self.conns.get(conn).map(|e| e.cfg.driver.clone())
    }

    /// The connection's configuration and secrets for a native tool, when it was connected
    /// at least once.
    pub(crate) fn tool_conn(&self, conn: &str) -> Option<ToolConn> {
        let entry = self.conns.get(conn)?;
        Some(ToolConn {
            cfg: entry.cfg.clone(),
            manifest: entry.driver.manifest().clone(),
            given: entry.secrets.clone(),
            resolved: entry.resolved.clone(),
            tunnel: entry.tunnel.clone(),
        })
    }

    /// The transaction state a session opens with: the user's settings for it, else the
    /// connection profile's. `None` when the connection is not connected.
    fn initial_tx(&self, key: &SessionKey) -> Option<TxState> {
        let profile = &self.conns.get(&key.conn)?.profile;
        let (mode, read_only) = self
            .prefs
            .get(key)
            .copied()
            .unwrap_or((profile.commit_mode, profile.read_only));
        Some(TxState::new(mode, read_only, now_ms()))
    }
}

/// How one page fetch ended.
enum PageEnd {
    /// `page_size` rows arrived; the stream may hold more.
    More,
    Exhausted,
    /// Cancel requested or result closed.
    Stopped,
    /// A later query on the session takes the stream over, or the stream was closed under
    /// this result.
    CutShort,
    Failed(DriverError),
}

pub(crate) struct Shared {
    pub(crate) operations: crate::operations::Operations,
    pub(crate) drivers_hub: crate::drivers::DriverHub,
    pub(crate) inner: Mutex<Inner>,
    pub(crate) catalog: CatalogCache,
    pub(crate) library: Library,
    pub(crate) tasks: crate::tasks::Tasks,
    pub(crate) scripts: Scripts,
    pub(crate) jobs: Jobs,
    pub(crate) db: DbQueue,
    /// Weak, so the engine's own sender alone keeps the runtime alive.
    commands: WeakUnboundedSender<Command>,
    repaint: Repaint,
    per_tab: AtomicBool,
    connect_timeout_s: AtomicU64,
    /// Rows a `fetch_all` may keep before it truncates the result.
    row_cap: AtomicUsize,
    /// Per-connection overrides of the settings above.
    conn_settings: Mutex<HashMap<String, ConnectionSettings>>,
    /// Display formats for text exports; `None` writes the raw values.
    pub(crate) export_formats: Mutex<Option<sqlr_core::DataFormats>>,
    pub(crate) ssh: SshContext,
    pub(crate) secret_store: Mutex<Option<SharedSecretStore>>,
    pub(crate) profiles: Mutex<ProfilesState>,
    /// Key-value state per connection: scans, values and consoles.
    pub(crate) kv: crate::kv::KvStates,
    /// Document results (`crate::docs`).
    pub(crate) docs: crate::docs::DocStates,
}

impl Shared {
    pub(crate) fn repaint(&self) {
        (self.repaint)();
    }

    /// The I/O handle of the connection's explorer session, if it is connecting or
    /// connected.
    pub(crate) fn live_io(&self, conn_id: &str) -> Option<Arc<SessionIo>> {
        let inner = self.inner.lock();
        inner
            .sessions
            .get(&SessionKey::meta(conn_id))
            .filter(|s| s.is_live())
            .map(|s| s.io.clone())
    }

    /// The session a result runs on.
    pub(crate) fn result_key(&self, q: QueryId) -> Option<SessionKey> {
        let inner = self.inner.lock();
        inner.results.get(&q).map(|e| e.key.clone())
    }

    pub(crate) fn result(&self, q: QueryId) -> Option<Arc<RwLock<QueryResult>>> {
        let inner = self.inner.lock();
        inner.results.get(&q).map(|e| e.result.clone())
    }

    pub(crate) fn send(&self, cmd: Command) {
        if let Some(tx) = self.commands.upgrade() {
            // Fails only once the runtime is gone, when there is nothing left to do.
            let _ = tx.send(cmd);
        }
    }

    /// Opens (or reopens) the session `key` from what `connect` kept, replacing (and
    /// retiring) the session it had. `None` when the connection is not connected.
    fn open_session(&self, inner: &mut Inner, key: &SessionKey) -> Option<Arc<SessionIo>> {
        let tx = inner.initial_tx(key)?;
        let entry = inner.conns.get(&key.conn)?;
        let io = SessionIo::new();
        let cmd = Command::Connect {
            key: key.clone(),
            driver: entry.driver.clone(),
            cfg: entry.cfg.clone(),
            secrets: entry.secrets.clone(),
            io: io.clone(),
            read_only: tx.read_only,
            tunnel: entry.tunnel.clone(),
        };
        let session = Session::new(SessionStatus::Connecting, io.clone(), tx);
        if let Some(old) = inner.sessions.insert(key.clone(), session) {
            self.retire(old);
        }
        self.send(cmd);
        Some(io)
    }

    /// The I/O of the session `key` when it is live; a tab session that does not exist yet,
    /// or any session that failed, is (re)opened.
    pub(crate) fn live_or_reopen(
        &self,
        inner: &mut Inner,
        key: &SessionKey,
    ) -> Option<Arc<SessionIo>> {
        let live = inner
            .sessions
            .get(key)
            .filter(|s| s.is_live())
            .map(|s| s.io.clone());
        let reopen = match inner.sessions.get(key) {
            None => key.slot != Slot::Meta,
            Some(s) => matches!(s.status, SessionStatus::Failed(_)),
        };
        match live {
            Some(io) => Some(io),
            None if reopen => self.open_session(inner, key),
            None => None,
        }
    }

    /// A new query id and the session `key`'s I/O (see `live_or_reopen`), for a document
    /// run (`crate::docs`).
    pub(crate) fn start_doc_query(&self, key: &SessionKey) -> (QueryId, Option<Arc<SessionIo>>) {
        let mut inner = self.inner.lock();
        inner.next_query += 1;
        let q = QueryId(inner.next_query);
        let io = self.live_or_reopen(&mut inner, key);
        (q, io)
    }

    /// Makes `stream` the rows result `q` of a document run on the session (its tabular
    /// results, such as `distinct` or ES|QL, are ordinary rows results: `Engine::result(q)`,
    /// fetched and closed like any other), then fetches its first page.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn adopt_rows(
        self: &Arc<Self>,
        q: QueryId,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        text: String,
        page_size: usize,
        stream: Box<dyn sqlr_core::RowStream>,
        slot: &mut StreamSlot,
    ) {
        let result = QueryResult {
            sql: text,
            database: None,
            status: QueryStatus::Running,
            rows: RowBuffer::new(stream.columns()),
            affected: None,
            elapsed: Duration::ZERO,
            page_size: page_size.max(1),
            cut_short: false,
            messages: Vec::new(),
        };
        {
            let mut inner = self.inner.lock();
            inner.results.insert(
                q,
                ResultEntry {
                    result: Arc::new(RwLock::new(result)),
                    key: key.clone(),
                    io: Some(io.clone()),
                    started: Instant::now(),
                    started_at_ms: now_ms(),
                    cancel_requested: false,
                    superseded: false,
                    recorded: false,
                    settled: Arc::new(Notify::new()),
                    outside_tx: false,
                },
            );
        }
        *slot = Some((q, stream));
        self.fetch_pages(q, slot, key, io, false, Vec::new()).await;
    }

    /// Interrupts and closes whatever a replaced or closed session still runs.
    pub(crate) fn retire(&self, old: Session) {
        old.io.connect_cancel.notify_one();
        if old.active.is_some()
            && let Some(handle) = old.cancel
        {
            self.send(Command::Cancel(handle));
        }
        self.send(Command::CloseStream {
            io: old.io,
            only: None,
        });
    }

    /// The secrets the connection's last `connect` resolved, when `cfg` and the given
    /// secrets are still the ones it kept.
    pub(crate) fn cached_secrets(
        &self,
        cfg: &ConnectionConfig,
        given: &Secrets,
    ) -> Option<Secrets> {
        let inner = self.inner.lock();
        let entry = inner.conns.get(&cfg.id)?;
        (entry.cfg == *cfg && entry.secrets == *given)
            .then(|| entry.resolved.clone())
            .flatten()
    }

    /// Keeps `resolved` for the connection's next sessions, if it still has `cfg`/`given`.
    pub(crate) fn cache_secrets(
        &self,
        cfg: &ConnectionConfig,
        given: &Secrets,
        resolved: &Secrets,
    ) {
        let mut inner = self.inner.lock();
        if let Some(entry) = inner.conns.get_mut(&cfg.id)
            && entry.cfg == *cfg
            && entry.secrets == *given
        {
            entry.resolved = Some(resolved.clone());
        }
    }

    fn connect_timeout(&self, conn: &str) -> Duration {
        let own = self
            .conn_settings
            .lock()
            .get(conn)
            .and_then(|s| s.connect_timeout_s);
        Duration::from_secs(
            own.map_or_else(|| self.connect_timeout_s.load(Ordering::Relaxed), u64::from),
        )
    }

    fn session_per_tab_for(&self, conn: &str) -> bool {
        let own = self
            .conn_settings
            .lock()
            .get(conn)
            .and_then(|s| s.session_per_tab);
        own.unwrap_or_else(|| self.per_tab.load(Ordering::Relaxed))
    }

    fn row_cap_for(&self, conn: &str) -> usize {
        match self.conn_settings.lock().get(conn).and_then(|s| s.row_cap) {
            Some(0) => usize::MAX,
            Some(cap) => cap,
            None => self.row_cap.load(Ordering::Relaxed),
        }
    }

    /// Creates the query's result and starts it on the session (`Failed` at once when
    /// there is no live session to run it on). A tab session opens on its first query, and
    /// a failed session reconnects. Executes on one session start in issue order. Returns
    /// the query and its signal for when the status first leaves `Running`.
    pub(crate) fn start_query(
        self: &Arc<Self>,
        key: &SessionKey,
        database: Option<String>,
        sql: String,
        page_size: usize,
        outside_tx: bool,
    ) -> (QueryId, Arc<Notify>) {
        let settled = Arc::new(Notify::new());
        let (q, connected) = {
            let mut inner = self.inner.lock();
            inner.next_query += 1;
            let q = QueryId(inner.next_query);
            let io = self.live_or_reopen(&mut inner, key);
            let status = match io {
                Some(_) => QueryStatus::Running,
                None => QueryStatus::Failed(not_connected()),
            };
            let result = QueryResult {
                sql,
                database: database.clone(),
                status,
                rows: RowBuffer::default(),
                affected: None,
                elapsed: Duration::ZERO,
                page_size: page_size.max(1),
                cut_short: false,
                messages: Vec::new(),
            };
            let connected = io.is_some();
            if let Some(io) = &io {
                // Review Focus 1: an in-flight fetch of an earlier query (even `fetch_all`)
                // gives the stream up at its next chunk boundary.
                for entry in inner.results.values_mut() {
                    if entry.io.as_ref().is_some_and(|i| Arc::ptr_eq(i, io)) {
                        entry.superseded = true;
                    }
                }
                self.send(Command::Execute {
                    q,
                    ticket: io.issue_ticket(),
                    database,
                    io: io.clone(),
                });
            }
            inner.results.insert(
                q,
                ResultEntry {
                    result: Arc::new(RwLock::new(result)),
                    key: key.clone(),
                    io,
                    started: Instant::now(),
                    started_at_ms: now_ms(),
                    cancel_requested: false,
                    superseded: false,
                    recorded: false,
                    settled: settled.clone(),
                    outside_tx,
                },
            );
            (q, connected)
        };
        if !connected {
            // Already `Failed`; this records it in the history.
            self.finish(q, QueryStatus::Failed(not_connected()));
        }
        self.repaint();
        (q, settled)
    }

    /// Interrupts a running query; a no-op if it is not running.
    pub(crate) fn cancel_query(&self, q: QueryId) {
        let mut inner = self.inner.lock();
        let inner = &mut *inner;
        let Some(entry) = inner.results.get_mut(&q) else {
            return;
        };
        if entry.result.read_recursive().status != QueryStatus::Running {
            return;
        }
        // A task that has not started the query yet sees this flag and skips it.
        entry.cancel_requested = true;
        let handle = inner
            .sessions
            .get(&entry.key)
            .filter(|s| s.active == Some(q))
            .and_then(|s| s.cancel.clone());
        if let Some(handle) = handle {
            self.send(Command::Cancel(handle));
        }
    }

    /// Forgets the result, interrupting it if running and closing its stream.
    pub(crate) fn close_query(&self, q: QueryId) {
        {
            let mut inner = self.inner.lock();
            let Some(entry) = inner.results.remove(&q) else {
                return;
            };
            if let Some(session) = inner.sessions.get(&entry.key)
                && session.active == Some(q)
                && let Some(handle) = &session.cancel
            {
                self.send(Command::Cancel(handle.clone()));
            }
            if !entry.recorded {
                // Its status will never leave `Running` now: wake whoever waits for it.
                entry.settled.notify_one();
            }
            if let Some(io) = entry.io {
                self.send(Command::CloseStream { io, only: Some(q) });
            }
        }
        self.repaint();
    }

    /// Whether fetching `q` must stop: cancelled or closed (`Stopped`), or a later query
    /// wants the stream (`CutShort`).
    fn stop(&self, q: QueryId) -> Option<PageEnd> {
        let inner = self.inner.lock();
        match inner.results.get(&q) {
            None => Some(PageEnd::Stopped),
            Some(e) if e.cancel_requested => Some(PageEnd::Stopped),
            Some(e) if e.superseded => Some(PageEnd::CutShort),
            Some(_) => None,
        }
    }

    async fn handle(self: Arc<Self>, cmd: Command) {
        use crate::operations::Target;
        use crate::{OperationKind as K, OperationState as S};
        let description = match &cmd {
            Command::Execute { q, .. } | Command::Fetch { q, .. } => self.result(*q).map(|r| {
                let r = r.read();
                let kind = if matches!(cmd, Command::Fetch { .. }) {
                    K::Fetch
                } else {
                    K::Query
                };
                (
                    kind,
                    r.sql.chars().take(80).collect(),
                    self.result_key(*q).map(|k| k.conn),
                    Target::Query(*q),
                )
            }),
            Command::RunScript(run) => Some((
                K::Script,
                "Run script".into(),
                Some(run.key.conn.clone()),
                Target::Script(run.id),
            )),
            _ => None,
        };
        let tracked = description.map(|(kind, label, conn, target)| {
            let id = self.operations.start(kind, label, conn, target.clone());
            (id, target)
        });
        let shared = self.clone();
        self.handle_command(cmd).await;
        if let Some((id, target)) = tracked {
            let state = match target {
                Target::Query(q) => shared
                    .result(q)
                    .map(|r| match &r.read().status {
                        QueryStatus::Cancelled => S::Cancelled,
                        QueryStatus::Failed(e) => S::Failed(e.to_string()),
                        _ => S::Done { ok: true },
                    })
                    .unwrap_or(S::Cancelled),
                Target::Script(id) => shared
                    .scripts
                    .state(id)
                    .map(|s| {
                        let s = s.read();
                        if s.cancelled {
                            S::Cancelled
                        } else if s.output.iter().any(|o| o.error.is_some()) {
                            S::Done { ok: false }
                        } else {
                            S::Done { ok: true }
                        }
                    })
                    .unwrap_or(S::Cancelled),
                _ => S::Done { ok: true },
            };
            shared.operations.finish(id, state);
            shared.repaint();
        }
    }

    async fn handle_command(self: Arc<Self>, cmd: Command) {
        match cmd {
            Command::Connect {
                key,
                driver,
                cfg,
                secrets,
                io,
                read_only,
                tunnel,
            } => {
                self.connect(key, driver, cfg, secrets, io, read_only, tunnel)
                    .await
            }
            Command::Browse { conn_id, path, io } => self.browse(conn_id, path, io).await,
            Command::Spawn(task) => task.await,
            Command::Execute {
                q,
                ticket,
                database,
                io,
            } => self.execute(q, ticket, database, io).await,
            Command::Fetch { q, all } => self.fetch(q, all).await,
            Command::Cancel(handle) => {
                // A failed cancel leaves the query running; there is no one to report it to.
                let _ = handle.cancel().await;
            }
            Command::CloseStream { io, only } => self.close_stream(io, only).await,
            Command::LoadCatalog {
                conn_id,
                database,
                need,
                io,
                token,
            } => {
                self.run_catalog_load(conn_id, database, need, io, token)
                    .await
            }
            Command::RunScript(run) => self.run_script(run).await,
            Command::Tx {
                key,
                io,
                ticket,
                op,
            } => self.run_tx_op(key, io, ticket, op).await,
            Command::Apply {
                job,
                key,
                io,
                ticket,
                changeset,
            } => self.run_apply(job, key, io, ticket, changeset).await,
            Command::Describe { key, io, table } => self.run_describe(key, io, table).await,
            Command::Transfer(run) => self.run_transfer(run).await,
            Command::Tool {
                job,
                conn,
                database,
                path,
                work,
            } => {
                let shared = self.clone();
                tokio::spawn(async move {
                    shared
                        .run_database_tool(job, conn, database, path, work)
                        .await;
                });
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn connect(
        self: Arc<Self>,
        key: SessionKey,
        driver: Arc<dyn Driver>,
        cfg: ConnectionConfig,
        secrets: Secrets,
        io: Arc<SessionIo>,
        read_only: bool,
        tunnel_slot: Arc<TunnelSlot>,
    ) {
        let started = Instant::now();
        let cancel = io.connect_cancel.clone();
        let operation = self.operations.start(
            crate::OperationKind::Connect,
            cfg.name.clone(),
            Some(cfg.id.clone()),
            crate::operations::Target::Connect(cancel.clone()),
        );
        let limit = self.connect_timeout(&cfg.id);
        let attempt = async {
            let secrets = self.connect_secrets(&cfg, driver.manifest(), secrets).await;
            let (cfg, tunnel) = self
                .tunnel_for(&cfg, &secrets, driver.manifest(), &tunnel_slot)
                .await?;
            let conn = driver.connect(&cfg, &secrets).await?;
            let info = conn.ping().await?;
            if read_only {
                apply_read_only(&conn, true).await?;
            }
            Ok((conn, info, tunnel))
        };
        let outcome: Result<_, DriverError> = match tokio::select! { biased; _=cancel.notified()=>Ok(Err(DriverError::new(ErrorKind::Cancelled,"Cancelled"))), result=tokio::time::timeout(limit,attempt)=>result }
        {
            Ok(outcome) => outcome,
            Err(_) => Err(DriverError::new(
                ErrorKind::Timeout,
                format!("Connecting timed out after {} s", limit.as_secs()),
            )),
        };
        let op_state = match &outcome {
            Ok(_) => crate::OperationState::Done { ok: true },
            Err(e) if e.kind == ErrorKind::Cancelled => crate::OperationState::Cancelled,
            Err(e) => crate::OperationState::Failed(e.to_string()),
        };
        let mut connected = false;
        {
            let mut inner = self.inner.lock();
            // A disconnect or a newer connect may have replaced this attempt meanwhile.
            let mut report = None;
            if let Some(session) = inner.session(&key, &io)
                && session.status == SessionStatus::Connecting
            {
                match outcome {
                    Ok((conn, info, tunnel)) => {
                        session.cancel = Some(conn.cancel_handle());
                        session.conn = Some(conn);
                        session.status = SessionStatus::Connected(info);
                        report = Some(ConnectReport {
                            ms: started.elapsed().as_millis() as u64,
                            host_key: tunnel.as_ref().map(|t| t.host_key.clone()),
                            tunnel_port: tunnel.as_ref().map(|t| t.port),
                        });
                        session.tunnel = tunnel;
                        connected = true;
                    }
                    Err(e) => session.status = SessionStatus::Failed(e),
                }
            }
            if let Some(report) = report.filter(|_| key.slot == Slot::Meta) {
                inner.reports.insert(key.conn.clone(), report);
            }
        }
        if connected && key.slot == Slot::Meta {
            self.touch_recent(&key.conn);
            // Register the refresh before releasing the session's waiting loads. Otherwise a
            // schema loaded between the two can already be `Ready` when this runs, and the
            // refresh fetches it a second time.
            self.refresh_catalog(&key.conn, &io);
        }
        io.finish_connect();
        self.operations.finish(operation, op_state);
        self.repaint();
    }

    /// The session's connection once its connect attempt finished, if it is `Connected`.
    pub(crate) async fn connected(
        &self,
        key: &SessionKey,
        io: &Arc<SessionIo>,
    ) -> Option<Arc<dyn Connection>> {
        io.connect_finished().await;
        let mut inner = self.inner.lock();
        let session = inner.session(key, io)?;
        if session.is_connected() {
            session.conn.clone()
        } else {
            None
        }
    }

    async fn browse(self: Arc<Self>, conn_id: String, path: NodePath, io: Arc<SessionIo>) {
        let key = SessionKey::meta(&conn_id);
        let cancel = Arc::new(Notify::new());
        let operation = self.operations.start(
            crate::OperationKind::Browse,
            format!("Browse {}", conn_id),
            Some(conn_id.clone()),
            crate::operations::Target::Read(cancel.clone()),
        );
        let request = async {
            match self.connected(&key, &io).await {
                None => LoadState::Failed(not_connected()),
                Some(conn) => match conn.browse(&path).await {
                    Ok(nodes) => LoadState::Ready(nodes),
                    Err(e) => {
                        self.on_error(&key, &io, &e);
                        LoadState::Failed(e)
                    }
                },
            }
        };
        let state = tokio::select! {biased;_ = cancel.notified()=>LoadState::Failed(DriverError::new(ErrorKind::Cancelled,"Cancelled")),state=request=>state};
        let outcome = match &state {
            LoadState::Ready(_) => crate::OperationState::Done { ok: true },
            LoadState::Failed(e) if e.kind == ErrorKind::Cancelled => {
                crate::OperationState::Cancelled
            }
            LoadState::Failed(e) => crate::OperationState::Failed(e.to_string()),
            _ => crate::OperationState::Done { ok: false },
        };
        self.operations.finish(operation, outcome);

        {
            let mut inner = self.inner.lock();
            if inner.session(&key, &io).is_none() {
                return;
            }
            // Disconnect and refresh drop entries; only the load still awaited is filled in.
            if let Some(entry) = inner
                .explorer
                .get_mut(&conn_id)
                .and_then(|cache| cache.get_mut(&path))
                && *entry == LoadState::Loading
            {
                *entry = state;
            }
        }
        self.repaint();
    }

    async fn execute(
        self: Arc<Self>,
        q: QueryId,
        ticket: u64,
        database: Option<String>,
        io: Arc<SessionIo>,
    ) {
        io.connect_finished().await;
        let turn = io.wait_turn(ticket).await;
        let mut slot = io.stream.lock().await;
        // The next execute may now queue on the stream lock, behind this one.
        drop(turn);
        let (key, sql) = {
            let inner = self.inner.lock();
            let Some(entry) = inner.results.get(&q) else {
                return;
            };
            (entry.key.clone(), entry.result.read().sql.clone())
        };
        // Review Focus 1: a session holds one open stream, so the previous one is closed.
        if let Some((prev, mut stream)) = slot.take() {
            let _ = stream.close().await;
            self.mark_complete(prev);
            self.repaint();
        }
        let conn = match self
            .query_connection(&key, &io, q, database.as_deref())
            .await
        {
            Ok(conn) => conn,
            Err(status) => {
                self.finish(q, status);
                return;
            }
        };
        let active = {
            let mut inner = self.inner.lock();
            let inner = &mut *inner;
            let Some(entry) = inner.results.get(&q) else {
                return;
            };
            let session = inner
                .sessions
                .get_mut(&key)
                .filter(|s| Arc::ptr_eq(&s.io, &io) && s.is_connected());
            if entry.cancel_requested {
                Err(QueryStatus::Cancelled)
            } else if let Some(session) = session {
                // The connection that runs the query is the one to cancel; for the
                // session's own database that is `session.conn`, otherwise a sibling.
                session.cancel = Some(conn.cancel_handle());
                session.active = Some(q);
                Ok(())
            } else {
                Err(QueryStatus::Failed(not_connected()))
            }
        };
        if let Err(status) = active {
            self.finish(q, status);
            return;
        }
        self.run_statement(q, &key, &io, &conn, &sql, &mut slot)
            .await;
        self.clear_active(&key, &io, q);
    }

    /// Runs `sql` under the session's transaction rules: refused when the session is
    /// read-only and it modifies, preceded by `begin_sql()` when the commit mode asks for a
    /// transaction, and followed by the transaction state's update.
    async fn run_statement(
        self: &Arc<Self>,
        q: QueryId,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        conn: &Arc<dyn Connection>,
        sql: &str,
        slot: &mut StreamSlot,
    ) {
        let Some(rel) = conn.relational() else {
            let e = DriverError::new(ErrorKind::Unsupported, "this connection does not run SQL");
            self.finish(q, QueryStatus::Failed(e));
            return;
        };
        let dialect = rel.dialect();
        let syntax = dialect.syntax();
        let mut stmt = Stmt::of(&syntax, sql);
        if dialect.begin_is_plsql()
            && matches!(
                stmt.class,
                sqlr_core::StatementClass::Transaction(sqlr_core::TxVerb::Begin)
            )
        {
            // Oracle's `BEGIN … END` is a block, not a transaction start: what it does is
            // what its body does.
            stmt.class = syntax.classify_script(sql);
        }
        if let Some(class) = dialect.statement_class(sql) {
            stmt.class = class;
        }
        let script_class = syntax.classify_script(sql);
        let rules = TxRules {
            ddl_commits: dialect.ddl_commits_transaction(),
            error_aborts: dialect.error_aborts_transaction(),
            nested_begin: dialect.nested_begin_increments_depth(),
        };
        let (read_only, begin, elsewhere) = {
            let mut inner = self.inner.lock();
            let outside_tx = inner.results.get(&q).is_some_and(|e| e.outside_tx)
                && stmt.class == sqlr_core::StatementClass::Read;
            match inner.session(key, io) {
                Some(s) => {
                    // The open transaction lives on one connection; a statement for another
                    // database would run outside it.
                    let elsewhere = s
                        .tx_conn
                        .as_ref()
                        .filter(|c| s.tx.is_open() && !same_connection(c, conn))
                        .map(|c| c.database_name().unwrap_or("another database").to_string());
                    (
                        s.tx.read_only,
                        !outside_tx && s.tx.needs_begin(&stmt, rules),
                        elsewhere,
                    )
                }
                None => (false, false, None),
            }
        };
        if read_only && script_class.modifies() {
            let e = DriverError::new(ErrorKind::Permission, READ_ONLY);
            self.finish(q, QueryStatus::Failed(e));
            return;
        }
        if let Some(db) = elsewhere {
            let e = DriverError::new(
                ErrorKind::Other,
                format!("A transaction is open on {db}; commit or roll back first"),
            );
            self.finish(q, QueryStatus::Failed(e));
            return;
        }
        if begin {
            match run_sql(conn, dialect.begin_sql()).await {
                Ok(()) => {
                    {
                        let mut inner = self.inner.lock();
                        if let Some(s) = inner.session(key, io) {
                            s.tx.open(now_ms());
                            s.tx_conn = Some(conn.clone());
                        }
                    }
                    self.add_message(
                        q,
                        RunMessage::new(MessageLevel::Info, "Transaction started"),
                    );
                }
                Err(e) => {
                    let lost = self.on_error(key, io, &e);
                    self.finish_with(q, error_status(e), lost.into_iter().collect());
                    return;
                }
            }
        }
        let outcome = rel.execute(sql).await;
        // §7.8: a DDL statement makes the cached catalog stale.
        if outcome.is_ok() && dialect.syntax().is_ddl(sql) {
            self.invalidate_catalog(&key.conn);
        }
        let mut notes = Vec::new();
        {
            let mut inner = self.inner.lock();
            if let Some(s) = inner.session(key, io) {
                let note = match &outcome {
                    Ok(_) => s.tx.after_success(&stmt, rules, now_ms()),
                    Err(e) if e.kind == ErrorKind::Connection => None,
                    Err(_) => s.tx.after_failure(&stmt, rules, now_ms()),
                };
                notes.extend(note);
                if !s.tx.is_open() {
                    s.tx_conn = None;
                } else if s.tx_conn.is_none() {
                    s.tx_conn = Some(conn.clone());
                }
            }
        }
        if outcome.is_ok() {
            notes.extend(
                rel.take_notices()
                    .await
                    .into_iter()
                    .map(|n| RunMessage::new(MessageLevel::Notice, n)),
            );
        }
        match outcome {
            Ok(Execution::Affected(n)) => {
                if let Some(result) = self.result(q) {
                    result.write().affected = Some(n);
                }
                self.finish_with(q, QueryStatus::Ready { more: false }, notes);
            }
            Ok(Execution::Rows(stream)) => {
                if let Some(result) = self.result(q) {
                    result.write().rows = RowBuffer::new(stream.columns());
                }
                *slot = Some((q, stream));
                self.fetch_pages(q, slot, key, io, false, notes).await;
            }
            Err(e) => {
                notes.extend(self.on_error(key, io, &e));
                self.finish_with(q, error_status(e), notes);
            }
        }
    }

    /// The session's connection, or (when `database` names another database of the same
    /// server) a connection to it, opened on first use (read-only when the session is).
    async fn query_connection(
        &self,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        q: QueryId,
        database: Option<&str>,
    ) -> Result<Arc<dyn Connection>, QueryStatus> {
        if !self.inner.lock().results.contains_key(&q) {
            return Err(QueryStatus::Cancelled);
        }
        self.session_connection(key, io, database)
            .await
            .map_err(QueryStatus::Failed)
    }

    /// The connected session's connection, or (when `database` names another database of
    /// the same server) a connection to it, opened on first use (read-only when the session
    /// is).
    pub(crate) async fn session_connection(
        &self,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        database: Option<&str>,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        let (base, existing, read_only) = {
            let inner = self.inner.lock();
            let Some(session) = inner
                .sessions
                .get(key)
                .filter(|s| Arc::ptr_eq(&s.io, io) && s.is_connected())
            else {
                return Err(not_connected());
            };
            let Some(base) = session.conn.clone() else {
                return Err(not_connected());
            };
            let existing = database.and_then(|db| session.databases.get(db)).cloned();
            (base, existing, session.tx.read_only)
        };
        let Some(db) = database else {
            return Ok(base);
        };
        if base.database_name() == Some(db) {
            return Ok(base);
        }
        if let Some(conn) = existing {
            return Ok(conn);
        }
        let conn = base.connect_database(db).await?;
        if read_only {
            apply_read_only(&conn, true).await?;
        }
        let mut inner = self.inner.lock();
        if let Some(session) = inner.session(key, io) {
            session.databases.insert(db.to_string(), conn.clone());
        }
        Ok(conn)
    }

    async fn fetch(self: Arc<Self>, q: QueryId, all: bool) {
        let (key, io) = {
            let inner = self.inner.lock();
            let Some(entry) = inner.results.get(&q) else {
                return;
            };
            let Some(io) = entry.io.clone() else {
                return;
            };
            (entry.key.clone(), io)
        };
        let mut slot = io.stream.lock().await;
        let connected = {
            let mut inner = self.inner.lock();
            match inner.session(&key, &io).filter(|s| s.is_connected()) {
                Some(session) => {
                    session.active = Some(q);
                    true
                }
                None => false,
            }
        };
        if connected {
            self.fetch_pages(q, &mut slot, &key, &io, all, Vec::new())
                .await;
            self.clear_active(&key, &io, q);
        } else {
            close_if_owned(&mut slot, q).await;
            self.mark_cut_short(q);
            self.finish(q, QueryStatus::Ready { more: false });
        }
    }

    /// Fetches one page (or, with `all`, every page) of `q` and sets the final status;
    /// `notes` join the messages when the status first leaves `Running`. The stream is
    /// closed once it will not be read again.
    async fn fetch_pages(
        self: &Arc<Self>,
        q: QueryId,
        slot: &mut StreamSlot,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        all: bool,
        mut notes: Vec<RunMessage>,
    ) {
        let cap = self.row_cap_for(&key.conn);
        loop {
            let mut truncated = false;
            let end = if !all {
                self.fetch_page(q, slot, None).await
            } else {
                let remaining = cap.saturating_sub(self.row_count(q));
                if remaining > 0 {
                    self.fetch_page(q, slot, Some(remaining)).await
                } else if let Some(end) = self.stop(q) {
                    end
                } else {
                    // At the cap: one row tells whether the stream had more.
                    match slot.as_mut().filter(|(owner, _)| *owner == q) {
                        None => PageEnd::CutShort,
                        Some((_, stream)) => match stream.next_chunk(1).await {
                            Ok(Some(chunk)) if !chunk.is_empty() => {
                                truncated = true;
                                PageEnd::Exhausted
                            }
                            Ok(_) => PageEnd::Exhausted,
                            Err(e) => PageEnd::Failed(e),
                        },
                    }
                }
            };
            let more = matches!(end, PageEnd::More);
            if !more {
                close_if_owned(slot, q).await;
            }
            let status = match end {
                PageEnd::More if all => QueryStatus::Running,
                PageEnd::More => QueryStatus::Ready { more: true },
                PageEnd::Exhausted => QueryStatus::Ready { more: false },
                PageEnd::CutShort => {
                    self.mark_cut_short(q);
                    QueryStatus::Ready { more: false }
                }
                PageEnd::Stopped => QueryStatus::Cancelled,
                PageEnd::Failed(e) => {
                    notes.extend(self.on_error(key, io, &e));
                    error_status(e)
                }
            };
            let running = status == QueryStatus::Running;
            self.finish_with(
                q,
                status,
                if running {
                    Vec::new()
                } else {
                    std::mem::take(&mut notes)
                },
            );
            if truncated {
                self.add_message(
                    q,
                    RunMessage::new(
                        MessageLevel::Warning,
                        format!("Result truncated at the row cap ({cap} rows)"),
                    ),
                );
            }
            if !(more && all) {
                return;
            }
        }
    }

    /// Rows `q` holds now.
    fn row_count(&self, q: QueryId) -> usize {
        self.result(q).map_or(0, |r| r.read().rows.row_count())
    }

    async fn fetch_page(&self, q: QueryId, slot: &mut StreamSlot, limit: Option<usize>) -> PageEnd {
        // The stream may have been closed meanwhile (new query, disconnect).
        let Some((_, stream)) = slot.as_mut().filter(|(owner, _)| *owner == q) else {
            return PageEnd::CutShort;
        };
        let (result, started) = {
            let inner = self.inner.lock();
            let Some(entry) = inner.results.get(&q) else {
                return PageEnd::Stopped;
            };
            (entry.result.clone(), entry.started)
        };
        let page_size = match limit {
            Some(limit) => result.read().page_size.min(limit),
            None => result.read().page_size,
        };
        let mut fetched = 0;
        while fetched < page_size {
            if let Some(end) = self.stop(q) {
                return end;
            }
            match stream.next_chunk(page_size - fetched).await {
                Ok(Some(chunk)) => {
                    if chunk.is_empty() {
                        break;
                    }
                    fetched += chunk.len();
                    let mut r = result.write();
                    r.rows.push_chunk(chunk);
                    r.elapsed = started.elapsed();
                }
                Ok(None) => return PageEnd::Exhausted,
                Err(e) => return PageEnd::Failed(e),
            }
        }
        // A cancel (or a later query) that arrived after the last chunk still ends this one.
        self.stop(q).unwrap_or(PageEnd::More)
    }

    pub(crate) async fn close_stream(&self, io: Arc<SessionIo>, only: Option<QueryId>) {
        let mut slot = io.stream.lock().await;
        let Some(owner) = slot.as_ref().map(|(owner, _)| *owner) else {
            return;
        };
        if only.is_some_and(|q| q != owner) {
            return;
        }
        close_if_owned(&mut slot, owner).await;
        self.mark_complete(owner);
        self.repaint();
    }

    /// Review Focus 2: a `Connection` error fails the session and closes its stream; an
    /// open transaction is gone with it, and the note saying so is returned.
    pub(crate) fn on_error(
        self: &Arc<Self>,
        key: &SessionKey,
        io: &Arc<SessionIo>,
        e: &DriverError,
    ) -> Option<RunMessage> {
        if e.kind != ErrorKind::Connection {
            return None;
        }
        let lost = {
            let mut inner = self.inner.lock();
            let session = inner.session(key, io).filter(|s| s.is_connected())?;
            session.status = SessionStatus::Failed(e.clone());
            session.conn = None;
            session.cancel = None;
            session.databases.clear();
            session.tx_conn = None;
            // A tunnel that died with the connection must not be reused on reconnect.
            session.tunnel = None;
            session.tx.lost(&e.message, now_ms())
        };
        // The caller may hold the stream lock, so the close runs as its own task.
        let this = self.clone();
        let io = io.clone();
        tokio::spawn(async move { this.close_stream(io, None).await });
        self.repaint();
        lost
    }

    pub(crate) fn add_message(&self, q: QueryId, message: RunMessage) {
        if let Some(result) = self.result(q) {
            result.write().messages.push(message);
        }
    }

    /// Sets the final status of a fetch or execute and the elapsed time. The first time
    /// the status leaves `Running`, the query is recorded in the history.
    pub(crate) fn finish(self: &Arc<Self>, q: QueryId, status: QueryStatus) {
        self.finish_with(q, status, Vec::new());
    }

    /// Like [`Shared::finish`]; the first time the status leaves `Running` the outcome
    /// message and then `notes` are added to the result's messages.
    fn finish_with(self: &Arc<Self>, q: QueryId, status: QueryStatus, notes: Vec<RunMessage>) {
        let entry = {
            let mut inner = self.inner.lock();
            inner.results.get_mut(&q).map(|e| {
                let first = !e.recorded && status != QueryStatus::Running;
                e.recorded |= first;
                let first = first.then(|| (e.key.conn.clone(), e.started_at_ms, e.settled.clone()));
                (e.result.clone(), e.started, first)
            })
        };
        if let Some((result, started, first)) = entry {
            let history = {
                let mut r = result.write();
                r.status = status;
                r.elapsed = started.elapsed();
                if first.is_some() {
                    let outcome = outcome_message(&r);
                    r.messages.push(outcome);
                    r.messages.extend(notes);
                }
                first.map(|(conn_id, at, settled)| (history_entry(conn_id, at, &r), settled))
            };
            // The status first left `Running`: record it, then wake a waiting script.
            if let Some((history, settled)) = history {
                if let Some(history) = history {
                    self.record_history(history);
                }
                settled.notify_one();
            }
        }
        self.repaint();
    }

    /// A result whose stream was closed under it is complete with no more rows, cut short.
    pub(crate) fn mark_complete(&self, q: QueryId) {
        if let Some(result) = self.result(q) {
            let mut r = result.write();
            if r.status == (QueryStatus::Ready { more: true }) {
                r.status = QueryStatus::Ready { more: false };
                r.cut_short = true;
            }
        }
    }

    /// Records that `q`'s stream ended before it was exhausted; `finish` sets the status.
    fn mark_cut_short(&self, q: QueryId) {
        if let Some(result) = self.result(q) {
            result.write().cut_short = true;
        }
    }

    fn clear_active(&self, key: &SessionKey, io: &Arc<SessionIo>, q: QueryId) {
        let mut inner = self.inner.lock();
        if let Some(session) = inner.session(key, io)
            && session.active == Some(q)
        {
            session.active = None;
        }
    }
}

/// Whether `a` and `b` are the same connection object.
pub(crate) fn same_connection(a: &Arc<dyn Connection>, b: &Arc<dyn Connection>) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
}

/// Turns the session's read-only mode on `conn` on or off: the dialect's statement for a
/// relational connection (when it has one), the driver's own switch for a document one.
pub(crate) async fn apply_read_only(
    conn: &Arc<dyn Connection>,
    on: bool,
) -> Result<(), DriverError> {
    if let Some(doc) = conn.document() {
        doc.set_read_only(on);
        return Ok(());
    }
    if let Some(kv) = conn.key_value() {
        kv.set_read_only(on);
        return Ok(());
    }
    let sql = conn
        .relational()
        .and_then(|rel| rel.dialect().read_only_sql(on));
    match sql {
        Some(sql) => run_sql(conn, &sql).await,
        None => Ok(()),
    }
}

/// The Messages line of a result whose status just left `Running`.
fn outcome_message(r: &QueryResult) -> RunMessage {
    let line = r
        .sql
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let line = match line.char_indices().nth(MESSAGE_SQL_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    };
    let ms = r.elapsed.as_millis();
    match (&r.status, r.affected) {
        (QueryStatus::Failed(e), _) => {
            RunMessage::new(MessageLevel::Error, format!("{line} · {}", e.message))
        }
        (QueryStatus::Cancelled, _) => {
            RunMessage::new(MessageLevel::Warning, format!("{line} · cancelled"))
        }
        (_, Some(n)) => RunMessage::new(
            MessageLevel::Info,
            format!("{line} · {n} rows affected · {ms} ms"),
        ),
        (status, None) => {
            let more = if *status == (QueryStatus::Ready { more: true }) {
                " (more available)"
            } else {
                ""
            };
            RunMessage::new(
                MessageLevel::Info,
                format!("{line} · {} rows{more} · {ms} ms", r.rows.row_count()),
            )
        }
    }
}

/// The status a failed execute or fetch ends with.
fn error_status(e: DriverError) -> QueryStatus {
    if e.kind == ErrorKind::Cancelled {
        QueryStatus::Cancelled
    } else {
        QueryStatus::Failed(e)
    }
}

/// Closes and drops the slot's stream if it belongs to `q`.
async fn close_if_owned(slot: &mut StreamSlot, q: QueryId) {
    if slot.as_ref().is_some_and(|(owner, _)| *owner == q)
        && let Some((_, mut stream)) = slot.take()
    {
        // Close errors are not actionable: the stream is dropped either way.
        let _ = stream.close().await;
    }
}
