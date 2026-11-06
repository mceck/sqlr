//! Per-connection session state.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use sqlr_core::{CancelHandle, Connection, DriverError, RowStream, ServerInfo};
use tokio::sync::watch;

use crate::result::QueryId;
use crate::ssh::Tunnel;
use crate::tx::TxState;

#[derive(Clone, Debug, PartialEq)]
pub enum SessionStatus {
    Connecting,
    Connected(ServerInfo),
    Failed(DriverError),
    Disconnected,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LoadState<T> {
    Loading,
    Ready(T),
    Failed(DriverError),
}

/// The session's one open stream and the query it belongs to.
pub(crate) type StreamSlot = Option<(QueryId, Box<dyn RowStream>)>;

/// The async side of a session. A new connect attempt creates a new `SessionIo`, so tasks
/// holding an old one can tell (by pointer) that their session was replaced.
pub(crate) struct SessionIo {
    /// Holding this lock serializes executes and fetches on the session.
    pub stream: tokio::sync::Mutex<StreamSlot>,
    pub connect_cancel: Arc<tokio::sync::Notify>,
    /// Becomes `true` once the connect attempt has finished, successfully or not.
    connect_done: watch::Sender<bool>,
    /// Executes start in issue order: each takes a ticket, and `turn` is the ticket
    /// allowed to take the stream lock next.
    next_ticket: AtomicU64,
    turn: watch::Sender<u64>,
    /// Set once the engine gave the connection up (the server never answered a cancel):
    /// every execute and fetch still holding or waiting for this I/O stops.
    abandoned: watch::Sender<Option<Abandoned>>,
    /// The connection was lost with a transaction open: statements still queued on this
    /// I/O belonged to it and must not run on a new connection.
    lost_transaction: AtomicBool,
}

/// How the engine gave a session's connection up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Abandoned {
    /// No transaction was open, so a statement still waiting for its turn may run on a
    /// new connection instead.
    pub rerun: bool,
}

/// Holds an execute's turn; dropping it lets the next ticket go.
pub(crate) struct Turn<'a>(&'a SessionIo);

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.0.turn.send_modify(|t| *t += 1);
    }
}

impl SessionIo {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            stream: tokio::sync::Mutex::new(None),
            connect_cancel: Arc::new(tokio::sync::Notify::new()),
            connect_done: watch::Sender::new(false),
            next_ticket: AtomicU64::new(0),
            turn: watch::Sender::new(0),
            abandoned: watch::Sender::new(None),
            lost_transaction: AtomicBool::new(false),
        })
    }

    pub fn finish_connect(&self) {
        self.connect_done.send_replace(true);
    }

    /// Waits until the connect attempt has finished.
    pub async fn connect_finished(&self) {
        let mut rx = self.connect_done.subscribe();
        // The sender lives in `self`, so this cannot fail while we hold `self`.
        let _ = rx.wait_for(|done| *done).await;
    }

    /// Gives the connection up (once): tasks still on this I/O stop.
    pub fn abandon(&self, how: Abandoned) {
        self.abandoned.send_if_modified(|current| {
            let first = current.is_none();
            if first {
                *current = Some(how);
            }
            first
        });
    }

    pub fn mark_lost_transaction(&self) {
        self.lost_transaction.store(true, Ordering::SeqCst);
    }

    pub fn lost_transaction(&self) -> bool {
        self.lost_transaction.load(Ordering::SeqCst)
    }

    pub fn is_abandoned(&self) -> bool {
        self.abandoned.borrow().is_some()
    }

    /// Waits until the connection is given up; never returns for a session that keeps it.
    pub async fn abandoned(&self) -> Abandoned {
        let mut rx = self.abandoned.subscribe();
        // The sender lives in `self`, so this cannot fail while we hold `self`.
        let how = rx.wait_for(Option::is_some).await.ok().and_then(|how| *how);
        match how {
            Some(how) => how,
            None => std::future::pending().await,
        }
    }

    /// The next execute ticket; every ticket must later be waited for with `wait_turn`.
    pub fn issue_ticket(&self) -> u64 {
        self.next_ticket.fetch_add(1, Ordering::Relaxed)
    }

    /// Waits until every execute issued before `ticket` has passed its turn on.
    pub async fn wait_turn(&self, ticket: u64) -> Turn<'_> {
        let mut rx = self.turn.subscribe();
        let _ = rx.wait_for(|t| *t == ticket).await;
        Turn(self)
    }
}

/// A connection to another database on the same server.
pub(crate) type DatabaseConn = Arc<dyn Connection>;

/// Which session of a connection: the explorer's (also catalog loads and jobs), an
/// editor tab's, or an MCP agent session's.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Meta,
    Tab(u64),
    /// An agent session, keyed by MCP session id (and kept per connection).
    Agent(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub conn: String,
    pub slot: Slot,
}

impl SessionKey {
    /// The connection's explorer session.
    pub fn meta(conn: &str) -> Self {
        Self {
            conn: conn.to_string(),
            slot: Slot::Meta,
        }
    }

    /// The session an MCP agent runs on: one per MCP session and connection.
    pub fn agent(conn: &str, agent: &str) -> Self {
        Self {
            conn: conn.to_string(),
            slot: Slot::Agent(agent.to_string()),
        }
    }
}

pub(crate) struct Session {
    pub status: SessionStatus,
    /// Set while `Connected`; also used synchronously for the dialect.
    pub conn: Option<Arc<dyn Connection>>,
    pub cancel: Option<Arc<dyn CancelHandle>>,
    /// Connections opened on demand to the server's other databases.
    pub databases: HashMap<String, DatabaseConn>,
    /// The query currently executing or fetching, if any.
    pub active: Option<QueryId>,
    pub io: Arc<SessionIo>,
    pub tx: TxState,
    /// The connection the open transaction runs on (the own one or another database's).
    pub tx_conn: Option<Arc<dyn Connection>>,
    /// Keeps the connection's SSH tunnel open while the session lives.
    pub tunnel: Option<Arc<Tunnel>>,
    /// When a query last finished on the session, or it connected: the idle ping waits for
    /// a quiet minute from here.
    pub last_used: std::time::Instant,
}

impl Session {
    pub fn new(status: SessionStatus, io: Arc<SessionIo>, tx: TxState) -> Self {
        Self {
            status,
            conn: None,
            cancel: None,
            databases: HashMap::new(),
            active: None,
            io,
            tx,
            tx_conn: None,
            tunnel: None,
            last_used: std::time::Instant::now(),
        }
    }

    pub fn is_live(&self) -> bool {
        matches!(
            self.status,
            SessionStatus::Connecting | SessionStatus::Connected(_)
        )
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.status, SessionStatus::Connected(_))
    }
}
