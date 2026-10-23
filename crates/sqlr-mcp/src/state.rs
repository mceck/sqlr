//! Shared state of one running MCP server.

use std::collections::HashMap;
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
    pub sessions: Mutex<HashMap<String, SessionInfo>>,
    pub inflight: Mutex<HashMap<String, usize>>,
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
            sessions: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
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

    pub fn begin_call(&self, id: &str) {
        self.touch_session(id);
        *self.inflight.lock().entry(id.to_string()).or_default() += 1;
    }

    pub fn end_call(&self, id: &str) {
        let mut inflight = self.inflight.lock();
        if let Some(count) = inflight.get_mut(id) {
            *count = count.saturating_sub(1);
        }
        drop(inflight);
        self.touch_session(id);
    }

    pub fn session_ids(&self) -> Vec<String> {
        self.sessions.lock().keys().cloned().collect()
    }

    pub fn remove_session(&self, id: &str) {
        self.sessions.lock().remove(id);
        self.inflight.lock().remove(id);
    }

    /// Sessions idle for 30 minutes with no in-flight call and no result used recently.
    pub fn idle_sessions(&self, idle_for: std::time::Duration) -> Vec<String> {
        let inflight = self.inflight.lock().clone();
        let now = Instant::now();
        self.sessions
            .lock()
            .iter()
            .filter(|(id, session)| {
                inflight.get(*id).copied().unwrap_or(0) == 0
                    && now.duration_since(session.last_activity) >= idle_for
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
