//! Shared state of one running MCP server.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use sqlr_engine::Engine;

use crate::host::AgentHost;
use crate::results::Results;

#[derive(Clone, Debug)]
pub struct ToolContext {
    pub session_id: String,
    pub client_name: String,
    pub client_version: String,
}

impl ToolContext {
    pub fn client_label(&self) -> String {
        if self.client_version.is_empty() {
            self.client_name.clone()
        } else {
            format!("{} {}", self.client_name, self.client_version)
        }
    }
}

pub struct SessionInfo {
    pub last_activity: Instant,
    pub client_name: String,
    pub client_version: String,
}

pub struct ServerState {
    pub engine: Arc<Engine>,
    pub host: Arc<dyn AgentHost>,
    pub results: Arc<Results>,
    pub port: u16,
    pub url: String,
    /// The in-memory verifier; rotation replaces it without rebinding.
    pub token: RwLock<String>,
    pub sessions: Arc<Mutex<HashMap<String, SessionInfo>>>,
    pub inflight: Arc<Inflight>,
    /// The Connection ids each MCP session created: that session may keep writing them
    /// while they are not exposed.
    pub created: Mutex<HashMap<String, HashSet<String>>>,
    /// How many tabs each MCP session opened or is opening.
    pub tabs_opened: TabCounts,
}

/// One MCP session opens at most this many tabs.
pub const MAX_AGENT_TABS: usize = 5;

/// The tool calls each MCP session has in flight.
#[derive(Default)]
pub struct Inflight(Mutex<HashMap<String, usize>>);

impl Inflight {
    fn begin(&self, session: &str) {
        *self.0.lock().entry(session.to_string()).or_default() += 1;
    }

    fn end(&self, session: &str) {
        if let Some(count) = self.0.lock().get_mut(session) {
            *count = count.saturating_sub(1);
        }
    }

    pub fn is_idle(&self, session: &str) -> bool {
        self.0.lock().get(session).copied().unwrap_or(0) == 0
    }

    fn remove(&self, session: &str) {
        self.0.lock().remove(session);
    }
}

/// Decrements a session's in-flight count when the tool call ends. It does so on drop too,
/// so a call whose future is dropped (client disconnect, aborted request) cannot leave the
/// session permanently busy and keep the idle sweep from reaping it. The end of a call is
/// activity: a call longer than the idle timeout must not get its session reaped at once.
pub(crate) struct CallGuard {
    inflight: Arc<Inflight>,
    sessions: Arc<Mutex<HashMap<String, SessionInfo>>>,
    session: String,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        if let Some(session) = self.sessions.lock().get_mut(&self.session) {
            session.last_activity = Instant::now();
        }
        self.inflight.end(&self.session);
    }
}

/// The tabs each MCP session opened, counted with the openings still in flight, so
/// concurrent calls of one session never go past [`MAX_AGENT_TABS`].
#[derive(Default)]
pub struct TabCounts(Mutex<HashMap<String, usize>>);

impl TabCounts {
    /// Takes one of `session`'s tab slots, or `None` at the cap. The check and the count
    /// happen under one lock; the slot is given back when the reservation is dropped
    /// without [`TabSlot::keep`].
    pub fn reserve(&self, session: &str) -> Option<TabSlot<'_>> {
        let mut counts = self.0.lock();
        let count = counts.entry(session.to_string()).or_default();
        if *count >= MAX_AGENT_TABS {
            return None;
        }
        *count += 1;
        Some(TabSlot {
            counts: self,
            session: session.to_string(),
            kept: false,
        })
    }

    /// Drops `session`'s count when the session ends.
    pub fn forget(&self, session: &str) {
        self.0.lock().remove(session);
    }
}

/// A reserved tab slot: counted while held, kept once a tab opened.
pub struct TabSlot<'a> {
    counts: &'a TabCounts,
    session: String,
    kept: bool,
}

impl TabSlot<'_> {
    /// A tab opened: the slot stays counted.
    pub fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for TabSlot<'_> {
    fn drop(&mut self) {
        if !self.kept
            && let Some(count) = self.counts.0.lock().get_mut(&self.session)
        {
            *count = count.saturating_sub(1);
        }
    }
}

impl ServerState {
    pub fn new(engine: Arc<Engine>, host: Arc<dyn AgentHost>, port: u16, token: String) -> Self {
        let url = format!("http://127.0.0.1:{port}/mcp");
        Self {
            engine,
            host,
            results: Arc::new(Results::default()),
            port,
            url,
            token: RwLock::new(token),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            inflight: Arc::new(Inflight::default()),
            created: Mutex::new(HashMap::new()),
            tabs_opened: TabCounts::default(),
        }
    }

    pub fn token(&self) -> String {
        self.token.read().clone()
    }

    pub fn rotate_token(&self, token: String) {
        *self.token.write() = token;
    }

    pub fn check_token(&self, candidate: &str) -> bool {
        crate::token::constant_time_eq(self.token().as_bytes(), candidate.as_bytes())
    }

    pub fn register_session(&self, id: &str, client_name: &str, client_version: &str) {
        self.sessions.lock().insert(
            id.to_string(),
            SessionInfo {
                last_activity: Instant::now(),
                client_name: client_name.to_string(),
                client_version: client_version.to_string(),
            },
        );
    }

    pub fn touch_session(&self, id: &str) {
        if let Some(session) = self.sessions.lock().get_mut(id) {
            session.last_activity = Instant::now();
        }
    }

    /// Counts a tool call on `id` until the returned guard drops.
    pub(crate) fn begin_call_guard(self: &Arc<Self>, id: &str) -> CallGuard {
        self.touch_session(id);
        self.inflight.begin(id);
        CallGuard {
            inflight: self.inflight.clone(),
            sessions: self.sessions.clone(),
            session: id.to_string(),
        }
    }

    pub fn session_ids(&self) -> Vec<String> {
        self.sessions.lock().keys().cloned().collect()
    }

    pub fn remove_session(&self, id: &str) {
        self.sessions.lock().remove(id);
        self.inflight.remove(id);
        self.created.lock().remove(id);
        self.tabs_opened.forget(id);
    }

    /// Reserves one of MCP session `session`'s tab slots; `None` at the cap.
    pub fn reserve_tab(&self, session: &str) -> Option<TabSlot<'_>> {
        self.tabs_opened.reserve(session)
    }

    /// Records that MCP session `session` created the Connection `connection`.
    pub fn record_created(&self, session: &str, connection: &str) {
        self.created
            .lock()
            .entry(session.to_string())
            .or_default()
            .insert(connection.to_string());
    }

    /// Whether MCP session `session` created the Connection `connection`.
    pub fn created_by(&self, session: &str, connection: &str) -> bool {
        self.created
            .lock()
            .get(session)
            .is_some_and(|ids| ids.contains(connection))
    }

    pub fn forget_created(&self, connection: &str) {
        for ids in self.created.lock().values_mut() {
            ids.remove(connection);
        }
    }

    /// Sessions idle for 30 minutes with no in-flight call and no result used recently.
    pub fn idle_sessions(&self, idle_for: std::time::Duration) -> Vec<String> {
        let now = Instant::now();
        self.sessions
            .lock()
            .iter()
            .filter(|(id, session)| {
                self.inflight.is_idle(id) && now.duration_since(session.last_activity) >= idle_for
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.lock().len()
    }

    pub fn context(&self, id: &str) -> ToolContext {
        let sessions = self.sessions.lock();
        let (name, version) = sessions
            .get(id)
            .map(|s| (s.client_name.clone(), s.client_version.clone()))
            .unwrap_or_default();
        ToolContext {
            session_id: id.to_string(),
            client_name: if name.is_empty() {
                "unknown client".into()
            } else {
                name
            },
            client_version: version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_call_releases_its_session() {
        let inflight = Arc::new(Inflight::default());
        inflight.begin("s");
        let call = CallGuard {
            inflight: inflight.clone(),
            sessions: Arc::default(),
            session: "s".into(),
        };
        assert!(!inflight.is_idle("s"));
        // The tool future was dropped (client gone): the guard still ends the call.
        drop(call);
        assert!(inflight.is_idle("s"));
    }

    #[test]
    fn the_end_of_a_long_call_counts_as_activity() {
        let inflight = Arc::new(Inflight::default());
        let sessions: Arc<Mutex<HashMap<String, SessionInfo>>> = Arc::default();
        let long_ago = Instant::now() - std::time::Duration::from_secs(3600);
        sessions.lock().insert(
            "s".into(),
            SessionInfo {
                last_activity: long_ago,
                client_name: "c".into(),
                client_version: "1".into(),
            },
        );
        inflight.begin("s");
        drop(CallGuard {
            inflight,
            sessions: sessions.clone(),
            session: "s".into(),
        });
        // A query or an approval that ran past the idle timeout just ended: the session
        // was used now, not when the call started.
        assert!(sessions.lock()["s"].last_activity > long_ago);
    }

    #[test]
    fn tab_slots_are_reserved_atomically_and_released_unless_kept() {
        let counts = TabCounts::default();
        let kept: Vec<_> = (0..MAX_AGENT_TABS - 1)
            .map(|_| counts.reserve("s1").expect("below the cap"))
            .collect();
        let last = counts.reserve("s1").expect("the fifth slot");
        assert!(
            counts.reserve("s1").is_none(),
            "a reservation holds the slot"
        );
        drop(last);
        let last = counts
            .reserve("s1")
            .expect("a dropped reservation frees its slot");
        for slot in kept {
            slot.keep();
        }
        last.keep();
        assert!(counts.reserve("s1").is_none(), "kept slots stay counted");
        assert!(
            counts.reserve("s2").is_some(),
            "each session has its own five"
        );
        counts.forget("s1");
        assert!(counts.reserve("s1").is_some());
    }
}
