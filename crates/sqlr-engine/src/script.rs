//! Script execution (spec §7.6): statements run one after another on the session's normal
//! execute path, each with its own result and history entry, and an output log.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use sqlr_core::DriverError;
use tokio::sync::Notify;

use crate::engine::Shared;
use crate::result::{QueryId, QueryStatus};
use crate::session::SessionKey;
use crate::tx::RunMessage;

/// What a script does after a failed statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptOnError {
    Stop,
    Continue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScriptId(pub u64);

/// One line of the script's output log.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputLine {
    /// 0-based index of the statement it is about.
    pub statement: usize,
    pub text: String,
    pub error: Option<DriverError>,
}

/// A script's progress, as the UI sees it.
#[derive(Clone, Debug, Default)]
pub struct ScriptState {
    /// The rows results, by statement index.
    pub results: Vec<(usize, QueryId)>,
    pub output: Vec<OutputLine>,
    /// The statement running now.
    pub running: Option<usize>,
    pub done: bool,
    pub cancelled: bool,
    /// Every statement's messages (outcome, notices, transaction notes), in run order.
    pub messages: Vec<RunMessage>,
}

/// A started statement's query and its signal for when its status first leaves `Running`.
type Started = (QueryId, Arc<Notify>);

/// A script for the runtime to run.
pub(crate) struct ScriptRun {
    pub(crate) id: ScriptId,
    pub(crate) key: SessionKey,
    /// The server database to run on, when not the connection's own.
    database: Option<String>,
    /// Statement 0, already started by `start_script`; `None` for an empty script.
    first: Option<Started>,
    /// Statements 1.. .
    rest: Vec<String>,
    page_size: usize,
    on_error: ScriptOnError,
}

/// What `start_statement` did.
enum Start {
    Started(Started),
    /// A cancel was requested: no more statements run.
    Cancelled,
    /// The script was closed.
    Closed,
}

struct ScriptEntry {
    state: Arc<RwLock<ScriptState>>,
    /// Every query the script started, in order.
    queries: Vec<QueryId>,
    /// The running statement's query.
    current: Option<QueryId>,
    cancel_requested: bool,
}

#[derive(Default)]
struct State {
    scripts: HashMap<ScriptId, ScriptEntry>,
    next_id: u64,
}

/// The scripts the engine holds. Lock order: this lock, then the engine's `inner`.
#[derive(Default)]
pub(crate) struct Scripts {
    state: Mutex<State>,
}

impl Scripts {
    pub fn state(&self, id: ScriptId) -> Option<Arc<RwLock<ScriptState>>> {
        let st = self.state.lock();
        st.scripts.get(&id).map(|e| e.state.clone())
    }

    /// Every query the script started, in statement order; empty once it is closed.
    pub fn queries(&self, id: ScriptId) -> Vec<QueryId> {
        let st = self.state.lock();
        st.scripts
            .get(&id)
            .map(|e| e.queries.clone())
            .unwrap_or_default()
    }
}

/// How one statement ended, read from its result.
struct Settled {
    status: QueryStatus,
    rows: usize,
    affected: Option<u64>,
    ms: u128,
    messages: Vec<RunMessage>,
}

impl Shared {
    /// Creates a script and starts its first statement at once, so it takes the
    /// session's next ticket (a later `execute` queues behind it). The rest runs as the
    /// returned `ScriptRun`.
    pub(crate) fn start_script(
        self: &Arc<Self>,
        key: &SessionKey,
        database: Option<String>,
        statements: Vec<String>,
        page_size: usize,
        on_error: ScriptOnError,
    ) -> (ScriptId, ScriptRun) {
        let id = {
            let mut st = self.scripts.state.lock();
            st.next_id += 1;
            let id = ScriptId(st.next_id);
            st.scripts.insert(
                id,
                ScriptEntry {
                    state: Arc::default(),
                    queries: Vec::new(),
                    current: None,
                    cancel_requested: false,
                },
            );
            id
        };
        let mut statements = statements.into_iter();
        let first = statements.next().and_then(|sql| {
            match self.start_statement(id, 0, key, database.clone(), sql, page_size) {
                Start::Started(started) => Some(started),
                // Nobody knows the id yet, so it can be neither cancelled nor closed.
                Start::Cancelled | Start::Closed => None,
            }
        });
        let run = ScriptRun {
            id,
            key: key.clone(),
            database,
            first,
            rest: statements.collect(),
            page_size,
            on_error,
        };
        (id, run)
    }

    /// Starts statement `i` on the session's normal execute path, under the scripts lock,
    /// so a cancel either sees its query or has already stopped the script.
    fn start_statement(
        self: &Arc<Self>,
        id: ScriptId,
        i: usize,
        key: &SessionKey,
        database: Option<String>,
        sql: String,
        page_size: usize,
    ) -> Start {
        let mut st = self.scripts.state.lock();
        let Some(entry) = st.scripts.get_mut(&id) else {
            return Start::Closed;
        };
        if entry.cancel_requested {
            return Start::Cancelled;
        }
        entry.state.write().running = Some(i);
        let (q, settled) = self.start_query(key, database, sql, page_size, false);
        entry.queries.push(q);
        entry.current = Some(q);
        Start::Started((q, settled))
    }

    /// Runs a script started by `start_script`: each statement waits for the previous
    /// one's first page or outcome.
    pub(crate) async fn run_script(self: Arc<Self>, run: ScriptRun) {
        let ScriptRun {
            id,
            key,
            database,
            mut first,
            rest,
            page_size,
            on_error,
        } = run;
        let Some(state) = self.scripts.state(id) else {
            return;
        };
        let mut rest = rest.into_iter();
        for i in 0.. {
            let k = i + 1;
            let started = match first.take() {
                Some(started) => started,
                None => {
                    let Some(sql) = rest.next() else {
                        break;
                    };
                    match self.start_statement(id, i, &key, database.clone(), sql, page_size) {
                        Start::Started(started) => started,
                        Start::Cancelled => {
                            cancelled(&mut state.write(), i);
                            break;
                        }
                        Start::Closed => return,
                    }
                }
            };
            let (q, settled) = started;
            self.repaint();
            // The statement's first page or outcome (or its result was closed).
            settled.notified().await;
            let cancel_requested = {
                let mut st = self.scripts.state.lock();
                let Some(entry) = st.scripts.get_mut(&id) else {
                    return;
                };
                entry.current = None;
                entry.cancel_requested
            };
            let mut s = state.write();
            // The statement's result was closed under it: the script ends cancelled.
            let Some(end) = self.settled(q) else {
                cancelled(&mut s, i);
                break;
            };
            s.messages.extend(end.messages.iter().cloned());
            let mut stop = false;
            let mut was_cancelled = false;
            match end.status {
                QueryStatus::Failed(e) => {
                    s.output.push(OutputLine {
                        statement: i,
                        text: format!("Statement {k}: {}", e.message),
                        error: Some(e),
                    });
                    if on_error == ScriptOnError::Stop {
                        s.output.push(OutputLine {
                            statement: i,
                            text: format!("Script stopped after statement {k}"),
                            error: None,
                        });
                        stop = true;
                    }
                }
                QueryStatus::Cancelled => was_cancelled = true,
                _ => {
                    let what = match end.affected {
                        Some(n) => format!("{n} rows affected"),
                        None => {
                            s.results.push((i, q));
                            format!("{} rows", end.rows)
                        }
                    };
                    s.output.push(OutputLine {
                        statement: i,
                        text: format!("Statement {k}: {what} · {} ms", end.ms),
                        error: None,
                    });
                }
            }
            if cancel_requested || was_cancelled {
                cancelled(&mut s, i);
                break;
            }
            if stop {
                break;
            }
        }
        {
            let mut s = state.write();
            s.running = None;
            s.done = true;
        }
        self.repaint();
    }

    pub(crate) fn cancel_script(&self, id: ScriptId) {
        let current = {
            let mut st = self.scripts.state.lock();
            let Some(entry) = st.scripts.get_mut(&id) else {
                return;
            };
            entry.cancel_requested = true;
            entry.current
        };
        if let Some(q) = current {
            self.cancel_query(q);
        }
    }

    pub(crate) fn close_script(&self, id: ScriptId) {
        let entry = self.scripts.state.lock().scripts.remove(&id);
        if let Some(entry) = entry {
            for q in entry.queries {
                self.close_query(q);
            }
        }
        self.repaint();
    }

    fn settled(&self, q: QueryId) -> Option<Settled> {
        let result = self.result(q)?;
        let r = result.read();
        Some(Settled {
            status: r.status.clone(),
            rows: r.rows.row_count(),
            affected: r.affected,
            ms: r.elapsed.as_millis(),
            messages: r.messages.clone(),
        })
    }
}

/// Ends the script as cancelled at statement `i`.
fn cancelled(s: &mut ScriptState, i: usize) {
    s.cancelled = true;
    s.output.push(OutputLine {
        statement: i,
        text: "Script cancelled".into(),
        error: None,
    });
}
