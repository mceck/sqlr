//! The catalog cache (spec §7.8): each connection's schema list and per-schema catalogs,
//! for autocomplete and the command palette. Held in memory, persisted in `sqlr.db`,
//! restored from it at startup and refreshed when the connection comes up.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use sqlr_core::{CatalogNeed, Connection, DriverError, ErrorKind, NodeKind, SchemaCatalog};
use sqlr_store::{Db, StoreError, StoredCatalog};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::engine::Shared;
use crate::session::{LoadState, SessionIo, SessionKey};

/// The catalog of one connection, as the UI sees it.
#[derive(Clone, Debug, Default)]
pub struct ConnCatalog {
    /// `None` = never requested.
    pub schemas: Option<LoadState<Vec<String>>>,
    pub schema: BTreeMap<String, LoadState<Arc<SchemaCatalog>>>,
    /// Loaded from `sqlr.db`, not yet refreshed this session.
    pub from_cache: bool,
}

/// A catalog entry's owner: a connection, and the database it describes (`None` for the
/// connection's own database).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CatalogKey {
    conn_id: String,
    database: Option<String>,
}

fn key(conn_id: &str, database: Option<&str>) -> CatalogKey {
    CatalogKey {
        conn_id: conn_id.to_string(),
        database: database.map(str::to_string),
    }
}

/// A `sqlr.db` operation, run on a blocking thread.
type DbJob = Box<dyn FnOnce(&Db) + Send>;

/// Runs `sqlr.db` operations one at a time, in the order they were pushed, each through
/// `spawn_blocking`. Pushing under the catalog lock keeps the file in step with memory.
pub(crate) struct DbQueue {
    tx: Mutex<Option<UnboundedSender<DbJob>>>,
}

impl DbQueue {
    pub fn new() -> (Self, UnboundedReceiver<DbJob>) {
        let (tx, rx) = unbounded_channel();
        let queue = Self {
            tx: Mutex::new(Some(tx)),
        };
        (queue, rx)
    }

    pub fn push(&self, job: impl FnOnce(&Db) + Send + 'static) {
        if let Some(tx) = &*self.tx.lock() {
            // Fails only once the worker is gone, at shutdown.
            let _ = tx.send(Box::new(job));
        }
    }

    /// Stops accepting jobs; the worker finishes the queued ones, then exits.
    pub fn close(&self) {
        self.tx.lock().take();
    }
}

/// Restores the catalog cache from `db`, then runs queued jobs until the queue closes.
/// The restore runs first, so no later write or delete can race it.
pub(crate) async fn run_db_worker(
    shared: Arc<Shared>,
    db: Arc<Db>,
    mut jobs: UnboundedReceiver<DbJob>,
) {
    let read = db.clone();
    match tokio::task::spawn_blocking(move || read.load_catalogs()).await {
        Ok(Ok(stored)) => shared.restore_catalogs(stored),
        Ok(Err(e)) => tracing::warn!("could not load the catalog cache: {e}"),
        Err(e) => tracing::warn!("loading the catalog cache panicked: {e}"),
    }
    while let Some(job) = jobs.recv().await {
        let db = db.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || job(&db)).await {
            tracing::warn!("a database job panicked: {e}");
        }
    }
}

/// What a finished load brought back.
enum Loaded {
    Schemas(Vec<String>),
    /// The requested schema name and its catalog.
    Schema(String, Arc<SchemaCatalog>),
}

/// An entry's state, without its data.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Loading,
    Ready,
    Failed,
}

fn kind_of<T>(state: &LoadState<T>) -> Kind {
    match state {
        LoadState::Loading => Kind::Loading,
        LoadState::Ready(_) => Kind::Ready,
        LoadState::Failed(_) => Kind::Failed,
    }
}

fn kind(cat: &ConnCatalog, need: &CatalogNeed) -> Option<Kind> {
    match need {
        CatalogNeed::Schemas => cat.schemas.as_ref().map(kind_of),
        CatalogNeed::Schema(name) => cat.schema.get(name).map(kind_of),
    }
}

/// Sets the entry to `Loading`, to `Failed(e)`, or (with `None`) removes it.
fn set_bare(cat: &mut ConnCatalog, need: &CatalogNeed, state: Option<Result<(), DriverError>>) {
    fn bare<T>(state: Result<(), DriverError>) -> LoadState<T> {
        match state {
            Ok(()) => LoadState::Loading,
            Err(e) => LoadState::Failed(e),
        }
    }
    match (need, state) {
        (CatalogNeed::Schemas, state) => cat.schemas = state.map(bare),
        (CatalogNeed::Schema(name), None) => {
            cat.schema.remove(name);
        }
        (CatalogNeed::Schema(name), Some(state)) => {
            cat.schema.insert(name.clone(), bare(state));
        }
    }
}

/// A load in flight: its token and the session it runs on.
struct Pending {
    token: u64,
    io: Arc<SessionIo>,
}

#[derive(Default)]
struct ConnState {
    snapshot: Arc<ConnCatalog>,
    pending: HashMap<CatalogNeed, Pending>,
    /// `Ready` entries not reloaded since the session (re)connected: restored from `sqlr.db`,
    /// or loaded before a reconnect (`reload_needs` marks every `Ready` entry stale).
    stale: HashSet<CatalogNeed>,
}

#[derive(Default)]
struct State {
    conns: BTreeMap<CatalogKey, ConnState>,
    next_token: u64,
    /// Whether the startup restore has landed.
    restored: bool,
    /// Connections invalidated before the restore landed: their stored rows are stale.
    invalidated_early: HashSet<String>,
}

#[derive(Default)]
pub(crate) struct CatalogCache {
    state: Mutex<State>,
    revision: AtomicU64,
}

impl CatalogCache {
    pub fn snapshot(&self, conn_id: &str, database: Option<&str>) -> Option<Arc<ConnCatalog>> {
        let st = self.state.lock();
        st.conns
            .get(&key(conn_id, database))
            .map(|c| c.snapshot.clone())
    }

    /// The catalogs of the connections' own databases.
    pub fn all(&self) -> Vec<(String, Arc<ConnCatalog>)> {
        let st = self.state.lock();
        st.conns
            .iter()
            .filter(|(k, _)| k.database.is_none())
            .map(|(k, c)| (k.conn_id.clone(), c.snapshot.clone()))
            .collect()
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    fn bump(&self) {
        self.revision.fetch_add(1, Ordering::SeqCst);
    }

    /// Registers a load of `need` on the session `io`, returning its token; `None` if the
    /// same session already loads it, if it is `Ready` and fresh, or if it is `Failed`
    /// and `retry_failed` is off (Review Focus 4: a missing schema is asked for once).
    /// A missing or failed entry becomes `Loading`; a stale `Ready` one stays visible
    /// until the new data lands.
    pub fn begin(
        &self,
        conn_id: &str,
        database: Option<&str>,
        need: &CatalogNeed,
        io: &Arc<SessionIo>,
        retry_failed: bool,
    ) -> Option<u64> {
        let mut st = self.state.lock();
        st.next_token += 1;
        let token = st.next_token;
        let conn = st.conns.entry(key(conn_id, database)).or_default();
        if conn
            .pending
            .get(need)
            .is_some_and(|p| Arc::ptr_eq(&p.io, io))
        {
            return None;
        }
        let kind = kind(&conn.snapshot, need);
        let ready = kind == Some(Kind::Ready);
        let fresh = ready && !conn.stale.contains(need);
        if fresh || (kind == Some(Kind::Failed) && !retry_failed) {
            return None;
        }
        conn.pending.insert(
            need.clone(),
            Pending {
                token,
                io: io.clone(),
            },
        );
        if !ready {
            set_bare(Arc::make_mut(&mut conn.snapshot), need, Some(Ok(())));
            self.bump();
        }
        Some(token)
    }

    /// Applies a finished load (`outcome` is `None` when the session was not connected)
    /// unless an invalidation or a newer load superseded it. Saves new data to `db`.
    /// Returns whether anything changed.
    fn finish(
        &self,
        conn_id: &str,
        database: Option<&str>,
        need: &CatalogNeed,
        token: u64,
        outcome: Option<Result<Loaded, DriverError>>,
        db: &DbQueue,
    ) -> bool {
        let mut st = self.state.lock();
        let Some(conn) = st.conns.get_mut(&key(conn_id, database)) else {
            return false;
        };
        if conn.pending.get(need).is_none_or(|p| p.token != token) {
            return false;
        }
        conn.pending.remove(need);
        let cat = Arc::make_mut(&mut conn.snapshot);
        // Whether the server answered for this entry, so it is no longer stale.
        let answered = match outcome {
            Some(Ok(loaded)) => {
                // Cross-database catalogs are kept in memory for this session only: the
                // stored rows are keyed by connection alone.
                if database.is_none() {
                    persist(db, conn_id, &loaded);
                }
                match loaded {
                    Loaded::Schemas(list) => cat.schemas = Some(LoadState::Ready(list)),
                    Loaded::Schema(name, c) => {
                        cat.schema.insert(name, LoadState::Ready(c));
                    }
                }
                true
            }
            // A dropped connection keeps the stale data visible.
            Some(Err(e)) if e.kind == ErrorKind::Connection => {
                if kind(cat, need) != Some(Kind::Ready) {
                    set_bare(cat, need, Some(Err(e)));
                }
                false
            }
            Some(Err(e)) => {
                set_bare(cat, need, Some(Err(e)));
                true
            }
            // No session to load on: the request is forgotten, the data kept.
            None => {
                if kind(cat, need) == Some(Kind::Loading) {
                    set_bare(cat, need, None);
                }
                false
            }
        };
        if answered {
            conn.stale.remove(need);
        }
        cat.from_cache = !conn.stale.is_empty();
        self.bump();
        true
    }

    /// Forgets the connection's catalogs (its own database and the other databases loaded
    /// for it) in memory and in `db`; loads in flight are dropped.
    fn invalidate(&self, conn_id: &str, db: &DbQueue) {
        let mut st = self.state.lock();
        st.conns.retain(|k, _| k.conn_id != conn_id);
        if !st.restored {
            st.invalidated_early.insert(conn_id.to_string());
        }
        let id = conn_id.to_string();
        db.push(move |db| {
            if let Err(e) = db.delete_catalog(&id) {
                tracing::warn!("could not delete the cached catalog: {e}");
            }
        });
        self.bump();
    }

    /// Fills in the catalogs restored from `sqlr.db`, as `from_cache`, for connections
    /// with nothing in memory yet. Returns the ids filled in.
    fn restore(&self, stored: Vec<StoredCatalog>) -> Vec<String> {
        let mut st = self.state.lock();
        st.restored = true;
        let early = std::mem::take(&mut st.invalidated_early);
        let mut restored = Vec::new();
        for s in stored {
            if early.contains(&s.conn_id) || st.conns.contains_key(&key(&s.conn_id, None)) {
                continue;
            }
            let mut cat = ConnCatalog::default();
            let mut stale = HashSet::new();
            if let Some(list) = s.schemas {
                cat.schemas = Some(LoadState::Ready(list));
                stale.insert(CatalogNeed::Schemas);
            }
            for c in s.schema {
                stale.insert(CatalogNeed::Schema(c.schema.clone()));
                cat.schema
                    .insert(c.schema.clone(), LoadState::Ready(Arc::new(c)));
            }
            cat.from_cache = !stale.is_empty();
            st.conns.insert(
                key(&s.conn_id, None),
                ConnState {
                    snapshot: Arc::new(cat),
                    pending: HashMap::new(),
                    stale,
                },
            );
            restored.push(s.conn_id);
        }
        if !restored.is_empty() {
            self.bump();
        }
        restored
    }

    /// For a session that just came up: marks every `Ready` entry stale (so `from_cache`
    /// means "not yet refreshed this session") and returns every entry to reload, with the
    /// database it belongs to (`None` for the connection's own): the `Ready` and `Failed`
    /// ones, and any still `Loading` on a session that went away.
    fn reload_needs(&self, conn_id: &str) -> Vec<(Option<String>, CatalogNeed)> {
        let mut st = self.state.lock();
        let keys: Vec<CatalogKey> = st
            .conns
            .keys()
            .filter(|k| k.conn_id == conn_id)
            .cloned()
            .collect();
        let mut out = Vec::new();
        for key in keys {
            let Some(conn) = st.conns.get_mut(&key) else {
                continue;
            };
            let cat = &conn.snapshot;
            let needs: Vec<CatalogNeed> = cat
                .schemas
                .iter()
                .map(|_| CatalogNeed::Schemas)
                .chain(cat.schema.keys().map(|s| CatalogNeed::Schema(s.clone())))
                .collect();
            for need in &needs {
                if kind(cat, need) == Some(Kind::Ready) {
                    conn.stale.insert(need.clone());
                }
            }
            let from_cache = !conn.stale.is_empty();
            if conn.snapshot.from_cache != from_cache {
                Arc::make_mut(&mut conn.snapshot).from_cache = from_cache;
                self.bump();
            }
            out.extend(needs.into_iter().map(|need| (key.database.clone(), need)));
        }
        out
    }
}

/// Queues the write of freshly loaded data; a failure is only logged.
fn persist(db: &DbQueue, conn_id: &str, loaded: &Loaded) {
    let id = conn_id.to_string();
    match loaded {
        Loaded::Schemas(list) => {
            let list = list.clone();
            db.push(move |db| warn_unsaved(db.save_catalog_schemas(&id, &list)));
        }
        Loaded::Schema(_, c) => {
            let c = c.clone();
            db.push(move |db| warn_unsaved(db.save_schema_catalog(&id, &c)));
        }
    }
}

fn warn_unsaved(result: Result<(), StoreError>) {
    if let Err(e) = result {
        tracing::warn!("could not save the catalog: {e}");
    }
}

/// Loads `need` from the server.
async fn fetch(conn: &dyn Connection, need: &CatalogNeed) -> Result<Loaded, DriverError> {
    match need {
        CatalogNeed::Schemas => {
            // When the explorer lists databases first, the connection's own schemas are
            // under its database node.
            let nodes = conn.browse(&conn.schema_path()).await?;
            let names = nodes
                .into_iter()
                .filter(|n| n.kind == NodeKind::Schema)
                .map(|n| n.name)
                .collect();
            Ok(Loaded::Schemas(names))
        }
        CatalogNeed::Schema(name) => match conn.relational() {
            Some(rel) => Ok(Loaded::Schema(
                name.clone(),
                Arc::new(rel.catalog(name).await?),
            )),
            None => Err(DriverError::new(
                ErrorKind::Unsupported,
                "this connection has no catalog",
            )),
        },
    }
}

impl Shared {
    /// Runs a load registered under `token`, once the session's connect attempt finished.
    /// `database` names the database to read the catalog from (`None` for the session's
    /// own), opening a connection to it when it is another one.
    pub(crate) async fn run_catalog_load(
        self: Arc<Self>,
        conn_id: String,
        database: Option<String>,
        need: CatalogNeed,
        io: Arc<SessionIo>,
        token: u64,
    ) {
        let key = SessionKey::meta(&conn_id);
        let cancel = Arc::new(tokio::sync::Notify::new());
        let operation = self.operations.start(
            crate::OperationKind::Browse,
            format!("Catalog {need:?}"),
            Some(conn_id.clone()),
            crate::operations::Target::Read(cancel.clone()),
        );
        let request = async {
            match self.connected(&key, &io).await {
                None => None,
                Some(_) => {
                    let result = match self
                        .session_connection(&key, &io, database.as_deref())
                        .await
                    {
                        Ok(conn) => fetch(&*conn, &need).await,
                        Err(e) => Err(e),
                    };
                    if let Err(e) = &result {
                        self.on_error(&key, &io, e);
                    }
                    Some(result)
                }
            }
        };
        let outcome = tokio::select! {biased;_ = cancel.notified()=>Some(Err(DriverError::new(ErrorKind::Cancelled,"Cancelled"))),outcome=request=>outcome};
        let state = match &outcome {
            Some(Ok(_)) => crate::OperationState::Done { ok: true },
            Some(Err(e)) if e.kind == ErrorKind::Cancelled => crate::OperationState::Cancelled,
            Some(Err(e)) => crate::OperationState::Failed(e.to_string()),
            None => crate::OperationState::Cancelled,
        };
        self.operations.finish(operation, state);
        if self.catalog.finish(
            &conn_id,
            database.as_deref(),
            &need,
            token,
            outcome,
            &self.db,
        ) {
            self.repaint();
        }
    }

    /// Reloads, in the background, every loaded or failed entry (of the connection's own
    /// database and of the others it has catalogs for) for the session `io` that just came
    /// up. Loaded data stays visible until the reload lands.
    pub(crate) fn refresh_catalog(self: &Arc<Self>, conn_id: &str, io: &Arc<SessionIo>) {
        for (database, need) in self.catalog.reload_needs(conn_id) {
            if let Some(token) = self
                .catalog
                .begin(conn_id, database.as_deref(), &need, io, true)
            {
                tokio::spawn(self.clone().run_catalog_load(
                    conn_id.to_string(),
                    database,
                    need,
                    io.clone(),
                    token,
                ));
            }
        }
    }

    /// Drops the connection's catalog from memory and from `sqlr.db`.
    pub(crate) fn invalidate_catalog(&self, conn_id: &str) {
        self.catalog.invalidate(conn_id, &self.db);
        self.repaint();
    }

    fn restore_catalogs(self: &Arc<Self>, stored: Vec<StoredCatalog>) {
        let restored = self.catalog.restore(stored);
        if restored.is_empty() {
            return;
        }
        self.repaint();
        // A session that came up before the restore landed refreshes now.
        for id in restored {
            if let Some(io) = self.live_io(&id) {
                self.refresh_catalog(&id, &io);
            }
        }
    }
}
