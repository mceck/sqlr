//! The local library: query history and snippets, persisted in `sqlr.db` through the
//! engine's ordered write queue. History is recorded once per query and searched per
//! channel; snippets live in memory and are written behind. Background persistence
//! failures become notices for the UI.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use sqlr_core::{DriverError, ErrorKind, HistoryEntry, HistoryOutcome, HistoryQuery, Snippet};
use sqlr_store::{MAX_HISTORY_SQL_BYTES, StoreError};

use crate::engine::Shared;
use crate::result::{QueryResult, QueryStatus};
use crate::session::LoadState;

/// An independent history search: the history panel and the command palette each keep
/// their own query and results.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HistoryChannel {
    Panel,
    Palette,
}

/// A channel's current query and its results, newest first.
#[derive(Clone, Debug)]
pub struct HistoryView {
    pub query: HistoryQuery,
    pub entries: LoadState<Arc<Vec<HistoryEntry>>>,
}

struct Channel {
    view: HistoryView,
    /// The token of the search that set `view.query`; earlier responses answer an
    /// earlier query, even when it had the same text.
    query_token: u64,
    /// The token of the response shown in `view.entries` (0 = none yet).
    shown_token: u64,
}

#[derive(Default)]
struct State {
    channels: HashMap<HistoryChannel, Channel>,
    next_token: u64,
    /// Sorted by `(folder, name, id)`.
    snippets: Arc<Vec<Snippet>>,
    /// Whether the startup load from `sqlr.db` has landed.
    snippets_loaded: bool,
    /// Snippets deleted before the startup load landed: their stored rows are gone.
    deleted_early: HashSet<String>,
    notices: Vec<String>,
    /// Recently connected connections, most recent first.
    recent: Arc<Vec<(String, i64)>>,
    /// Connections forgotten before the startup load landed.
    recent_forgotten: HashSet<String>,
}

/// Recent connections read at startup.
const RECENT_KEEP: usize = 50;

/// Sessions that are no saved connection: the connection dialog's tests and the demo
/// database's setup use ids with a `:`, which saved connections (UUIDs) never have.
fn is_transient(conn_id: &str) -> bool {
    conn_id.contains(':')
}

#[derive(Default)]
pub(crate) struct Library {
    state: Mutex<State>,
}

/// Unix milliseconds now.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The history entry for a query whose status just left `Running`; `None` while it runs.
/// Only the SQL text, connection id, timing and outcome are kept: never secrets.
pub(crate) fn history_entry(
    conn_id: String,
    started_at_ms: i64,
    r: &QueryResult,
) -> Option<HistoryEntry> {
    // A query that names or sets a secret must not land in the plaintext history file, even
    // though the connection's own credentials live in the OS keychain.
    if crate::docs::may_hold_secret(&r.sql) {
        return None;
    }
    let outcome = match &r.status {
        QueryStatus::Running => return None,
        QueryStatus::Ready { .. } => match r.affected {
            Some(n) => HistoryOutcome::Affected(n),
            None => HistoryOutcome::Rows(r.rows.row_count() as u64),
        },
        QueryStatus::Cancelled => HistoryOutcome::Cancelled,
        QueryStatus::Failed(e) => HistoryOutcome::Failed(e.message.clone()),
    };
    // Only what the store keeps is copied: this runs under the result's write guard.
    let sql = &r.sql[..r.sql.floor_char_boundary(MAX_HISTORY_SQL_BYTES)];
    Some(HistoryEntry {
        id: 0,
        conn_id,
        database: r.database.clone(),
        sql: sql.to_string(),
        started_at_ms,
        duration_ms: u64::try_from(r.elapsed.as_millis()).unwrap_or(u64::MAX),
        outcome,
    })
}

fn sort_snippets(list: &mut [Snippet]) {
    list.sort_by(|a, b| (&a.folder, &a.name, &a.id).cmp(&(&b.folder, &b.name, &b.id)));
}

impl Library {
    /// Makes `q` the channel's query (results reset to `Loading` when it changed) and
    /// returns the token of the search to run.
    fn begin_search(&self, channel: HistoryChannel, q: &HistoryQuery) -> u64 {
        let mut st = self.state.lock();
        st.next_token += 1;
        let token = st.next_token;
        let ch = st.channels.entry(channel).or_insert_with(|| Channel {
            view: HistoryView {
                query: q.clone(),
                entries: LoadState::Loading,
            },
            query_token: token,
            shown_token: 0,
        });
        if ch.view.query != *q {
            ch.view.query = q.clone();
            ch.view.entries = LoadState::Loading;
            ch.query_token = token;
        }
        token
    }

    /// A fresh token and the current query of every channel, to search again.
    fn begin_refresh(&self) -> Vec<(HistoryChannel, HistoryQuery, u64)> {
        let mut st = self.state.lock();
        let st = &mut *st;
        st.channels
            .iter()
            .map(|(channel, ch)| {
                st.next_token += 1;
                (*channel, ch.view.query.clone(), st.next_token)
            })
            .collect()
    }

    /// Shows a search's results unless the channel moved on to another query or already
    /// shows a newer response. Returns whether anything changed.
    fn finish_search(
        &self,
        channel: HistoryChannel,
        q: &HistoryQuery,
        token: u64,
        result: Result<Vec<HistoryEntry>, StoreError>,
    ) -> bool {
        let mut st = self.state.lock();
        let Some(ch) = st.channels.get_mut(&channel) else {
            return false;
        };
        if ch.view.query != *q || token < ch.query_token || token <= ch.shown_token {
            return false;
        }
        ch.view.entries = match result {
            Ok(entries) => LoadState::Ready(Arc::new(entries)),
            Err(e) => LoadState::Failed(DriverError::new(ErrorKind::Other, e.to_string())),
        };
        ch.shown_token = token;
        true
    }

    /// Merges the snippets loaded at startup into memory. Snippets saved or deleted
    /// before the load landed keep their in-memory state.
    fn restore_snippets(&self, stored: Vec<Snippet>) {
        let mut st = self.state.lock();
        let st = &mut *st;
        st.snippets_loaded = true;
        let deleted = std::mem::take(&mut st.deleted_early);
        let list = Arc::make_mut(&mut st.snippets);
        let known: HashSet<String> = list.iter().map(|s| s.id.clone()).collect();
        list.extend(
            stored
                .into_iter()
                .filter(|s| !known.contains(&s.id) && !deleted.contains(&s.id)),
        );
        sort_snippets(list);
    }
}

impl Shared {
    /// Queues the startup load of the snippets; it runs before any later write.
    pub(crate) fn load_snippets(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.db.push(move |db| {
            let result = db.list_snippets();
            let Some(shared) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(stored) => {
                    shared.library.restore_snippets(stored);
                    shared.repaint();
                }
                Err(e) => shared.notice(format!("Could not load snippets: {e}")),
            }
        });
    }

    /// Queues the startup load of the recent connections. Connects recorded before it
    /// lands keep their (newer) time.
    pub(crate) fn load_recent(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.db.push(move |db| {
            let result = db.recent(RECENT_KEEP);
            let Some(shared) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(stored) => {
                    {
                        let mut st = shared.library.state.lock();
                        let st = &mut *st;
                        let forgotten = std::mem::take(&mut st.recent_forgotten);
                        let list = Arc::make_mut(&mut st.recent);
                        for (id, at) in stored {
                            if !forgotten.contains(&id) && !list.iter().any(|(c, _)| *c == id) {
                                list.push((id, at));
                            }
                        }
                        list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                    }
                    shared.repaint();
                }
                Err(e) => shared.notice(format!("Could not load recent connections: {e}")),
            }
        });
    }

    pub(crate) fn recent(&self) -> Arc<Vec<(String, i64)>> {
        self.library.state.lock().recent.clone()
    }

    /// Records that `conn_id` just connected (not for transient sessions), in memory at
    /// once and in `sqlr.db` behind.
    pub(crate) fn touch_recent(self: &Arc<Self>, conn_id: &str) {
        if is_transient(conn_id) {
            return;
        }
        let at = now_ms();
        {
            let mut st = self.library.state.lock();
            let list = Arc::make_mut(&mut st.recent);
            list.retain(|(c, _)| c != conn_id);
            list.insert(0, (conn_id.to_string(), at));
            let id = conn_id.to_string();
            self.persist_recent(move |db| db.touch_recent(&id, at));
        }
        self.repaint();
    }

    /// Forgets a connection's recent use (it was deleted).
    pub(crate) fn forget_recent(self: &Arc<Self>, conn_id: &str) {
        {
            let mut st = self.library.state.lock();
            Arc::make_mut(&mut st.recent).retain(|(c, _)| c != conn_id);
            st.recent_forgotten.insert(conn_id.to_string());
            let id = conn_id.to_string();
            self.persist_recent(move |db| db.forget_recent(&id));
        }
        self.repaint();
    }

    fn persist_recent(
        self: &Arc<Self>,
        write: impl FnOnce(&sqlr_store::Db) -> Result<(), StoreError> + Send + 'static,
    ) {
        let weak = Arc::downgrade(self);
        self.db.push(move |db| {
            if let Err(e) = write(db)
                && let Some(shared) = weak.upgrade()
            {
                shared.notice(format!("Could not save recent connections: {e}"));
            }
        });
    }

    pub(crate) fn search_history(self: &Arc<Self>, channel: HistoryChannel, q: HistoryQuery) {
        let token = self.library.begin_search(channel, &q);
        self.run_search(channel, q, token);
        self.repaint();
    }

    pub(crate) fn history(&self, channel: HistoryChannel) -> Option<HistoryView> {
        let st = self.library.state.lock();
        st.channels.get(&channel).map(|ch| ch.view.clone())
    }

    /// Stores a query's history entry; once stored, every open search runs again so it
    /// shows the new entry.
    pub(crate) fn record_history(self: &Arc<Self>, entry: HistoryEntry) {
        let weak = Arc::downgrade(self);
        self.db.push(move |db| {
            let result = db.add_history(&entry);
            let Some(shared) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(_) => shared.refresh_history(),
                Err(e) => shared.notice(format!("Could not save history: {e}")),
            }
        });
    }

    /// Deletes the connection's history entries (`None`: every connection's), then runs
    /// every open search again.
    pub(crate) fn clear_history(self: &Arc<Self>, conn_id: Option<String>) {
        let weak = Arc::downgrade(self);
        self.db.push(move |db| {
            let result = db.clear_history(conn_id.as_deref());
            let Some(shared) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(()) => shared.refresh_history(),
                Err(e) => shared.notice(format!("Could not clear history: {e}")),
            }
        });
    }

    pub(crate) fn snippets(&self) -> Arc<Vec<Snippet>> {
        self.library.state.lock().snippets.clone()
    }

    /// Inserts the snippet, or replaces the one with the same id, in memory at once;
    /// the write to `sqlr.db` follows in the background.
    pub(crate) fn save_snippet(self: &Arc<Self>, s: Snippet) {
        if crate::docs::may_hold_secret(&s.sql) {
            self.notice("A snippet that may contain a password was not saved".into());
            return;
        }
        {
            let mut st = self.library.state.lock();
            let list = Arc::make_mut(&mut st.snippets);
            match list.iter_mut().find(|old| old.id == s.id) {
                Some(old) => *old = s.clone(),
                None => list.push(s.clone()),
            }
            sort_snippets(list);
            // Queued under the lock, so the file sees the writes in memory's order.
            self.persist_snippet(move |db| db.save_snippet(&s));
        }
        self.repaint();
    }

    pub(crate) fn delete_snippet(self: &Arc<Self>, id: &str) {
        {
            let mut st = self.library.state.lock();
            Arc::make_mut(&mut st.snippets).retain(|s| s.id != id);
            if !st.snippets_loaded {
                st.deleted_early.insert(id.to_string());
            }
            let id = id.to_string();
            self.persist_snippet(move |db| db.delete_snippet(&id));
        }
        self.repaint();
    }

    pub(crate) fn take_notices(&self) -> Vec<String> {
        std::mem::take(&mut self.library.state.lock().notices)
    }

    fn persist_snippet(
        self: &Arc<Self>,
        write: impl FnOnce(&sqlr_store::Db) -> Result<(), StoreError> + Send + 'static,
    ) {
        let weak = Arc::downgrade(self);
        self.db.push(move |db| {
            if let Err(e) = write(db)
                && let Some(shared) = weak.upgrade()
            {
                shared.notice(format!("Could not save snippet: {e}"));
            }
        });
    }

    /// Runs every channel's current query again.
    fn refresh_history(self: &Arc<Self>) {
        for (channel, q, token) in self.library.begin_refresh() {
            self.run_search(channel, q, token);
        }
    }

    /// Queues the search, so it sees every history entry stored before it.
    fn run_search(self: &Arc<Self>, channel: HistoryChannel, q: HistoryQuery, token: u64) {
        let weak: Weak<Self> = Arc::downgrade(self);
        self.db.push(move |db| {
            let result = db.search_history(&q);
            if let Some(shared) = weak.upgrade()
                && shared.library.finish_search(channel, &q, token, result)
            {
                shared.repaint();
            }
        });
    }

    /// Reports a background persistence failure to the UI.
    pub(crate) fn notice(&self, message: String) {
        tracing::error!(op = "store", "{message}");
        self.library.state.lock().notices.push(message);
        self.repaint();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlr_core::RowBuffer;

    use super::*;

    #[test]
    fn history_entry_copies_at_most_the_stored_sql() {
        // 2 MiB of a 2-byte char, offset by one byte so the limit falls inside a char.
        let sql = format!("x{}", "é".repeat(MAX_HISTORY_SQL_BYTES));
        let r = QueryResult {
            memory: Default::default(),
            spill: Arc::new(crate::result_spill::SpillStore::default()),
            sql,
            database: None,
            status: QueryStatus::Ready { more: false },
            rows: RowBuffer::default(),
            affected: Some(1),
            elapsed: Duration::from_millis(3),
            page_size: 100,
            cut_short: false,
            messages: Vec::new(),
        };
        let e = history_entry("c".into(), 1, &r).expect("an entry");
        assert!(
            e.sql.len() <= MAX_HISTORY_SQL_BYTES,
            "{} bytes",
            e.sql.len()
        );
        assert_eq!(e.sql.len(), MAX_HISTORY_SQL_BYTES - 1);
        assert!(r.sql.starts_with(&e.sql));
    }

    #[test]
    fn password_bearing_sql_is_not_recorded() {
        for sql in [
            "CREATE USER app IDENTIFIED BY 'S3cret'",
            "ALTER ROLE app WITH PASSWORD 'S3cret'",
            "CREATE LOGIN app WITH PASSWORD = 'S3cret'",
            "SELECT api_key FROM settings",
        ] {
            let r = QueryResult {
                memory: Default::default(),
                spill: Arc::new(crate::result_spill::SpillStore::default()),
                sql: sql.into(),
                database: None,
                status: QueryStatus::Ready { more: false },
                rows: RowBuffer::default(),
                affected: Some(0),
                elapsed: Duration::from_millis(1),
                page_size: 100,
                cut_short: false,
                messages: Vec::new(),
            };
            assert!(history_entry("c".into(), 1, &r).is_none(), "recorded {sql}");
        }
    }
}
