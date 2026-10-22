//! The Redis console: one per tab, each with its own database, output and history.
//!
//! A console runs its commands one after another in the order they were entered (its own
//! [`Gate`]), on connections the driver keeps apart from the browser's, and never takes the
//! session's turn: a blocking `BLPOP` cannot hold up a read-only switch or a catalog load.
//! What it retains is bounded in entries and in bytes.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use sqlr_core::{
    DriverError, HistoryEntry, HistoryOutcome, HistoryQuery, RespValue, StatementClass,
    redact_command, split_command_line,
};
use tokio::sync::watch;

use super::{ConnKv, key_value};
use crate::engine::{Command, Engine, Shared};
use crate::job::{JobControl, JobId, JobKind};
use crate::library::now_ms;
use crate::session::SessionKey;

/// Finished entries a console keeps, at most.
pub(super) const CONSOLE_ENTRIES: usize = 500;
/// Bytes of output a console keeps, at most; the oldest entries go first.
pub(super) const CONSOLE_BYTES: usize = 8 << 20;
/// Lines of history in memory and restored from `sqlr.db`.
const CONSOLE_HISTORY: usize = 500;
/// Elements of one reply the console keeps.
const REPLY_ELEMENTS: usize = 5_000;
/// Bytes of one bulk reply the console keeps.
const REPLY_BYTES: usize = 64 * 1024;
/// Bytes of one whole reply the console keeps.
const REPLY_TOTAL_BYTES: usize = 1 << 20;
/// What the console shows for a line that does not parse: it may hold a secret.
pub(super) const UNPARSED: &str = "<unparsed> ***";

/// The console of one connection as the UI reads it.
#[derive(Clone, Debug, Default)]
pub struct ConsoleSnapshot {
    /// Finished commands, oldest first, with the line as shown (secrets hidden).
    pub output: Arc<Vec<Arc<(String, RespValue)>>>,
    /// Lines entered, oldest first.
    pub history: Arc<Vec<String>>,
    /// Commands queued in an open `MULTI` block.
    pub queued: Option<usize>,
    /// The logical database commands run on.
    pub db: u32,
    /// A command is still running.
    pub running: bool,
}

/// Lets the commands of one console run in the order they were issued.
#[derive(Debug)]
pub(crate) struct Gate {
    next: AtomicU64,
    turn: watch::Sender<u64>,
}

/// Holds a command's turn; dropping it lets the next one go.
pub(super) struct Turn<'a>(&'a Gate);

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.0.turn.send_modify(|t| *t += 1);
    }
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            next: AtomicU64::new(0),
            turn: watch::Sender::new(0),
        })
    }

    fn ticket(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    async fn wait_turn(&self, ticket: u64) -> Turn<'_> {
        let mut rx = self.turn.subscribe();
        let _ = rx.wait_for(|t| *t == ticket).await;
        Turn(self)
    }
}

struct Entry {
    id: u64,
    line: String,
    /// `None` while the command runs.
    done: Option<Arc<(String, RespValue)>>,
    /// What the entry retains, roughly.
    bytes: usize,
}

pub(super) struct Console {
    entries: Vec<Entry>,
    shown: Arc<Vec<Arc<(String, RespValue)>>>,
    history: Arc<Vec<String>>,
    history_requested: bool,
    /// Commands of the open `MULTI` block.
    block: Option<Vec<Vec<Vec<u8>>>>,
    pub(super) db: u32,
    next_id: u64,
    gate: Arc<Gate>,
}

impl Default for Console {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            shown: Arc::default(),
            history: Arc::default(),
            history_requested: false,
            block: None,
            db: 0,
            next_id: 0,
            gate: Gate::new(),
        }
    }
}

/// Roughly what a reply holds in memory.
pub(super) fn reply_bytes(reply: &RespValue) -> usize {
    match reply {
        RespValue::Bulk(b) => b.len() + 24,
        RespValue::Simple(s) | RespValue::Error(s) | RespValue::Verbatim(s) => s.len() + 24,
        RespValue::Array(items) => 24 + items.iter().map(reply_bytes).sum::<usize>(),
        RespValue::Map(pairs) => {
            24 + pairs
                .iter()
                .map(|(k, v)| reply_bytes(k) + reply_bytes(v))
                .sum::<usize>()
        }
        _ => 24,
    }
}

impl Console {
    fn rebuild(&mut self) {
        // Entries show once every earlier one has finished, so output keeps command order.
        // Only pointers are cloned.
        let done = self.entries.iter().map_while(|e| e.done.clone()).collect();
        self.shown = Arc::new(done);
    }

    /// Drops the oldest finished entries past the entry and byte limits (never the newest).
    fn evict(&mut self) {
        loop {
            let finished = self.entries.iter().filter(|e| e.done.is_some()).count();
            let over = finished > CONSOLE_ENTRIES || self.retained_bytes() > CONSOLE_BYTES;
            if !over || finished <= 1 {
                return;
            }
            if let Some(at) = self.entries.iter().position(|e| e.done.is_some()) {
                self.entries.remove(at);
            }
        }
    }

    /// Roughly what the console retains as output.
    pub(super) fn retained_bytes(&self) -> usize {
        self.entries.iter().map(|e| e.bytes).sum()
    }

    #[cfg(test)]
    pub(super) fn finished(&self) -> usize {
        self.entries.iter().filter(|e| e.done.is_some()).count()
    }

    pub(super) fn push(&mut self, line: String, reply: Option<RespValue>) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let bytes = line.len() + reply.as_ref().map_or(0, reply_bytes) + 48;
        let done = reply.map(|r| Arc::new((line.clone(), r)));
        self.entries.push(Entry {
            id,
            line,
            done,
            bytes,
        });
        self.evict();
        self.rebuild();
        id
    }

    pub(super) fn complete(&mut self, id: u64, reply: RespValue) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.bytes = entry.line.len() + reply_bytes(&reply) + 48;
            entry.done = Some(Arc::new((entry.line.clone(), reply)));
        }
        self.evict();
        self.rebuild();
    }

    pub(super) fn add_history(&mut self, line: &str) {
        if self.history.last().is_some_and(|last| last == line) {
            return;
        }
        let history = Arc::make_mut(&mut self.history);
        history.push(line.to_string());
        if history.len() > CONSOLE_HISTORY {
            history.remove(0);
        }
    }

    pub(super) fn running(&self) -> bool {
        self.entries.iter().any(|e| e.done.is_none())
    }

    #[cfg(test)]
    pub(super) fn history(&self) -> &[String] {
        &self.history
    }
}

/// What a reply may still take from the console's budget.
struct Budget {
    elements: usize,
    bytes: usize,
}

impl Budget {
    fn new() -> Self {
        Budget {
            elements: REPLY_ELEMENTS,
            bytes: REPLY_TOTAL_BYTES,
        }
    }

    fn exhausted(&self) -> bool {
        self.elements == 0 || self.bytes == 0
    }
}

/// `reply` cut down to what the console keeps.
pub(super) fn limit_reply(reply: RespValue) -> RespValue {
    limit(reply, &mut Budget::new())
}

fn limit(reply: RespValue, budget: &mut Budget) -> RespValue {
    match reply {
        RespValue::Bulk(bytes) => {
            let keep = REPLY_BYTES.min(budget.bytes);
            budget.bytes = budget.bytes.saturating_sub(bytes.len().min(keep));
            if bytes.len() > keep {
                let more = bytes.len() - keep;
                let mut cut = bytes[..keep].to_vec();
                cut.extend_from_slice(format!("… ({more} more bytes)").as_bytes());
                RespValue::Bulk(cut)
            } else {
                RespValue::Bulk(bytes)
            }
        }
        RespValue::Array(items) => {
            let total = items.len();
            let mut kept = Vec::new();
            for item in items {
                if budget.exhausted() {
                    break;
                }
                budget.elements -= 1;
                kept.push(limit(item, budget));
            }
            if kept.len() < total {
                kept.push(RespValue::Simple(format!(
                    "… {} more entries",
                    total - kept.len()
                )));
            }
            RespValue::Array(kept)
        }
        RespValue::Map(pairs) => {
            let total = pairs.len();
            let mut kept = Vec::new();
            for (k, v) in pairs {
                if budget.exhausted() {
                    break;
                }
                budget.elements -= 1;
                kept.push((limit(k, budget), limit(v, budget)));
            }
            if kept.len() < total {
                kept.push((
                    RespValue::Simple(format!("… {} more entries", total - kept.len())),
                    RespValue::Nil,
                ));
            }
            RespValue::Map(kept)
        }
        other => other,
    }
}

fn history_outcome(reply: &RespValue) -> HistoryOutcome {
    match reply {
        RespValue::Error(message) => HistoryOutcome::Failed(message.clone()),
        RespValue::Nil => HistoryOutcome::Rows(0),
        RespValue::Array(items) => HistoryOutcome::Rows(items.len() as u64),
        RespValue::Map(pairs) => HistoryOutcome::Rows(pairs.len() as u64),
        _ => HistoryOutcome::Rows(1),
    }
}

/// A console command waiting to run.
struct ConsoleRun {
    pub conn: String,
    pub tab: u64,
    pub ticket: u64,
    pub gate: Arc<Gate>,
    pub id: u64,
    /// The line as shown: arguments quoted, secrets hidden.
    pub line: String,
    pub args: Vec<Vec<u8>>,
    pub db: u32,
    /// The queued commands of an `EXEC`.
    pub block: Option<Vec<Vec<Vec<u8>>>>,
}

fn console_of(c: &mut ConnKv, tab: u64) -> &mut Console {
    c.consoles.entry(tab).or_default()
}

impl Engine {
    /// The class of console `line` on the connection; `None` when it is empty or does not
    /// parse, or the connection is not a connected key-value one.
    pub fn kv_classify(&self, conn_id: &str, line: &str) -> Option<StatementClass> {
        let args = split_command_line(line).ok().filter(|a| !a.is_empty())?;
        let conn = self.shared.meta_connection(conn_id)?;
        Some(conn.key_value()?.classify(&args))
    }

    /// The number of logical databases; `None` when not connected.
    pub fn kv_databases(&self, conn_id: &str) -> Option<u32> {
        let conn = self.shared.meta_connection(conn_id)?;
        Some(conn.key_value()?.databases())
    }

    /// Opens the console of tab `tab` on database `db` (`SELECT` in it changes only this
    /// console).
    pub fn kv_set_console_db(&self, conn_id: &str, tab: u64, db: u32) {
        self.shared.kv.with(conn_id, |c| console_of(c, tab).db = db);
    }

    /// The console of tab `tab`; the first call also restores its history from `sqlr.db`,
    /// in the background.
    pub fn kv_console(&self, conn_id: &str, tab: u64) -> ConsoleSnapshot {
        self.shared.ensure_console(conn_id, tab);
        self.shared.kv.with(conn_id, |c| {
            let console = console_of(c, tab);
            ConsoleSnapshot {
                output: console.shown.clone(),
                history: console.history.clone(),
                queued: console.block.as_ref().map(Vec::len),
                db: console.db,
                running: console.running(),
            }
        })
    }

    /// Forgets the console's output (its history stays).
    pub fn kv_clear_console(&self, conn_id: &str, tab: u64) {
        self.shared.kv.with(conn_id, |c| {
            let console = console_of(c, tab);
            console.entries.retain(|e| e.done.is_none());
            console.rebuild();
        });
    }

    /// Forgets the console of a closed tab.
    pub fn kv_close_console(&self, conn_id: &str, tab: u64) {
        self.shared.kv.with(conn_id, |c| {
            c.consoles.remove(&tab);
        });
    }

    /// Interrupts the commands the connection's consoles are running.
    pub fn kv_cancel_console(&self, conn_id: &str) {
        let handle = {
            let inner = self.shared.inner.lock();
            inner
                .sessions
                .get(&SessionKey::meta(conn_id))
                .and_then(|s| s.cancel.clone())
        };
        if let Some(handle) = handle {
            self.shared.send(Command::Cancel(handle));
        }
    }

    /// Runs a console line like `redis-cli` does: arguments split with quotes, `MULTI`
    /// opens a block that `EXEC` runs and `DISCARD` drops, `SELECT` switches this console's
    /// database. Commands run one after another; their output keeps the order they were
    /// entered in. Secrets (`AUTH`, `ACL SETUSER`, …) never reach the output or the
    /// history, and neither does a line that does not parse.
    pub fn kv_command(&self, conn_id: &str, tab: u64, line: &str) -> Option<JobId> {
        self.shared.console_command(conn_id, tab, line)
    }
}

impl Shared {
    fn ensure_console(self: &Arc<Self>, conn_id: &str, tab: u64) {
        let first = self.kv.with(conn_id, |c| {
            let console = console_of(c, tab);
            let first = !console.history_requested;
            // Marked at once so the load runs once.
            console.history_requested = true;
            first
        });
        if !first {
            return;
        }
        let weak = Arc::downgrade(self);
        let conn = conn_id.to_string();
        self.db.push(move |db| {
            let query = HistoryQuery {
                text: String::new(),
                conn_id: Some(conn.clone()),
                limit: CONSOLE_HISTORY,
            };
            let Ok(found) = db.search_history(&query) else {
                return;
            };
            let Some(shared) = weak.upgrade() else {
                return;
            };
            // Newest first in the store; oldest first in the console.
            let mut stored: Vec<String> = found.into_iter().rev().map(|e| e.sql).collect();
            stored.dedup();
            shared.kv.with(&conn, |c| {
                let Some(console) = c.consoles.get_mut(&tab) else {
                    return;
                };
                let session = std::mem::take(&mut console.history);
                let mut all = stored;
                for line in session.iter() {
                    if all.last() != Some(line) {
                        all.push(line.clone());
                    }
                }
                let excess = all.len().saturating_sub(CONSOLE_HISTORY);
                all.drain(..excess);
                console.history = Arc::new(all);
            });
            shared.repaint();
        });
    }

    /// Records a console line in the history of `sqlr.db`.
    fn record_console(
        self: &Arc<Self>,
        conn_id: &str,
        line: &str,
        started_at: i64,
        ms: u64,
        reply: &RespValue,
    ) {
        self.record_history(HistoryEntry {
            id: 0,
            conn_id: conn_id.to_string(),
            database: None,
            sql: line.to_string(),
            started_at_ms: started_at,
            duration_ms: ms,
            outcome: history_outcome(reply),
        });
    }

    fn console_command(self: &Arc<Self>, conn_id: &str, tab: u64, line: &str) -> Option<JobId> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        self.ensure_console(conn_id, tab);
        let started_at = now_ms();
        let args = match split_command_line(line) {
            Ok(args) if !args.is_empty() => args,
            Ok(_) => return None,
            Err(message) => {
                // The line cannot be redacted argument by argument, and may hold a
                // password: it is neither shown nor recorded.
                let reply = RespValue::Error(format!("ERR {message}"));
                self.kv.with(conn_id, |c| {
                    console_of(c, tab).push(UNPARSED.to_string(), Some(reply));
                });
                self.repaint();
                return None;
            }
        };
        let shown = redact_command(&args);
        let name = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
        let databases = self
            .meta_connection(conn_id)
            .and_then(|c| Some(c.key_value()?.databases()))
            .unwrap_or(16);
        let ok = || RespValue::Simple("OK".into());
        let error = |m: &str| RespValue::Error(format!("ERR {m}"));
        // Local outcomes: the reply is known now.
        let local: Option<RespValue> = self.kv.with(conn_id, |c| {
            let console = console_of(c, tab);
            console.add_history(&shown);
            match name.as_str() {
                "MULTI" if console.block.is_some() => Some(error("MULTI calls can not be nested")),
                "MULTI" => {
                    console.block = Some(Vec::new());
                    Some(ok())
                }
                "DISCARD" if console.block.is_none() => Some(error("DISCARD without MULTI")),
                "DISCARD" => {
                    console.block = None;
                    Some(ok())
                }
                "EXEC" if console.block.is_none() => Some(error("EXEC without MULTI")),
                "WATCH" | "UNWATCH" => Some(error("WATCH is not supported in the console")),
                "SELECT" => Some(
                    match args
                        .get(1)
                        .and_then(|a| std::str::from_utf8(a).ok()?.parse::<u32>().ok())
                    {
                        Some(n) if n < databases && args.len() == 2 => {
                            console.db = n;
                            ok()
                        }
                        _ => error("DB index is out of range"),
                    },
                ),
                _ => match &mut console.block {
                    Some(block) if name != "EXEC" => {
                        block.push(args.clone());
                        Some(RespValue::Simple("QUEUED".into()))
                    }
                    _ => None,
                },
            }
        });
        if let Some(reply) = local {
            self.kv.with(conn_id, |c| {
                console_of(c, tab).push(shown.clone(), Some(reply.clone()));
            });
            self.record_console(conn_id, &shown, started_at, 0, &reply);
            self.repaint();
            return None;
        }
        if self.live_io(conn_id).is_none() {
            let reply = RespValue::Error("ERR not connected".into());
            self.kv.with(conn_id, |c| {
                console_of(c, tab).push(shown.clone(), Some(reply));
            });
            self.repaint();
            return None;
        }
        let run = self.kv.with(conn_id, |c| {
            let console = console_of(c, tab);
            let block = if name == "EXEC" {
                console.block.take()
            } else {
                None
            };
            let gate = console.gate.clone();
            ConsoleRun {
                conn: conn_id.to_string(),
                tab,
                ticket: gate.ticket(),
                gate,
                id: console.push(shown.clone(), None),
                line: shown,
                args,
                db: console.db,
                block,
            }
        });
        let id = self.spawn_kv(JobKind::KvConsole, move |shared, job| async move {
            shared.run_console(run, job).await
        });
        self.repaint();
        Some(id)
    }

    async fn run_console(
        self: Arc<Self>,
        run: ConsoleRun,
        job: JobControl,
    ) -> Result<(), DriverError> {
        let ConsoleRun {
            conn,
            tab,
            ticket,
            gate,
            id,
            line,
            args,
            db,
            block,
        } = run;
        // One command at a time, in the order they were entered.
        let turn = gate.wait_turn(ticket).await;
        let started = Instant::now();
        let started_at = now_ms();
        let mut outcome = Ok(());
        let reply = match self.kv_connection(&conn).await {
            Err(e) => {
                outcome = Err(e.clone());
                RespValue::Error(e.message)
            }
            Ok(connection) => match key_value(&connection) {
                Err(e) => {
                    outcome = Err(e.clone());
                    RespValue::Error(e.message)
                }
                Ok(kv) => {
                    // `Engine::cancel_job` interrupts the command through the driver.
                    job.set_cancel(Some(connection.cancel_handle()));
                    let result = match block {
                        Some(commands) => kv.exec_block(db, commands).await,
                        None => kv.command(db, args).await,
                    };
                    job.set_cancel(None);
                    match result {
                        Ok(reply) => reply,
                        Err(e) => {
                            outcome = Err(e.clone());
                            RespValue::Error(e.message)
                        }
                    }
                }
            },
        };
        drop(turn);
        let reply = limit_reply(reply);
        let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.record_console(&conn, &line, started_at, ms, &reply);
        self.kv.with(&conn, |c| {
            if let Some(console) = c.consoles.get_mut(&tab) {
                console.complete(id, reply);
            }
        });
        self.repaint();
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_replies_are_cut_down() {
        let items = (0..REPLY_ELEMENTS + 10)
            .map(|i| RespValue::Int(i as i64))
            .collect();
        let RespValue::Array(kept) = limit_reply(RespValue::Array(items)) else {
            panic!("an array stays an array")
        };
        assert_eq!(kept.len(), REPLY_ELEMENTS + 1);
        assert_eq!(
            kept[REPLY_ELEMENTS],
            RespValue::Simple("… 10 more entries".into())
        );
        let RespValue::Bulk(bytes) = limit_reply(RespValue::Bulk(vec![b'x'; REPLY_BYTES + 5]))
        else {
            panic!("a bulk stays a bulk")
        };
        assert!(bytes.ends_with(b"(5 more bytes)"));
    }

    #[test]
    fn a_reply_of_many_big_values_stays_under_its_byte_budget() {
        let items = (0..1_000)
            .map(|_| RespValue::Bulk(vec![b'x'; REPLY_BYTES]))
            .collect();
        let kept = limit_reply(RespValue::Array(items));
        assert!(reply_bytes(&kept) < REPLY_TOTAL_BYTES + 3 * REPLY_BYTES);
        let RespValue::Array(kept) = kept else {
            panic!()
        };
        assert!(kept.len() < 100, "{}", kept.len());
    }

    #[test]
    fn console_output_keeps_command_order() {
        let mut console = Console::default();
        let slow = console.push("BLPOP q 0".into(), None);
        console.push("PING".into(), Some(RespValue::Simple("PONG".into())));
        // The finished PING waits behind the running command.
        assert!(console.shown.is_empty());
        assert!(console.running());
        console.complete(slow, RespValue::Nil);
        assert_eq!(console.shown.len(), 2);
        assert_eq!(console.shown[0].0, "BLPOP q 0");
        assert!(!console.running());
    }

    #[test]
    fn retained_output_has_a_byte_budget_and_evicts_the_oldest() {
        let mut console = Console::default();
        let big = RespValue::Bulk(vec![b'x'; 1 << 20]);
        for i in 0..40 {
            console.push(format!("GET key{i}"), Some(big.clone()));
        }
        assert!(
            console.retained_bytes() <= CONSOLE_BYTES + (1 << 20),
            "{}",
            console.retained_bytes()
        );
        assert!(console.finished() < 40);
        // The newest entry is always kept; the oldest went first.
        assert_eq!(console.shown.last().unwrap().0, "GET key39");
        assert_ne!(console.shown.first().unwrap().0, "GET key0");
    }

    #[test]
    fn retained_output_is_capped_in_entries() {
        let mut console = Console::default();
        for i in 0..CONSOLE_ENTRIES + 20 {
            console.push(format!("PING {i}"), Some(RespValue::Nil));
        }
        assert_eq!(console.finished(), CONSOLE_ENTRIES);
        assert_eq!(console.shown.len(), CONSOLE_ENTRIES);
    }

    #[test]
    fn rebuilding_the_snapshot_shares_the_entries() {
        let mut console = Console::default();
        console.push("A".into(), Some(RespValue::Nil));
        let before = console.shown.clone();
        console.push("B".into(), Some(RespValue::Nil));
        assert!(Arc::ptr_eq(&before[0], &console.shown[0]));
    }

    #[test]
    fn history_skips_repeats_and_is_capped() {
        let mut console = Console::default();
        console.add_history("PING");
        console.add_history("PING");
        console.add_history("GET k");
        assert_eq!(console.history(), ["PING", "GET k"]);
        for i in 0..CONSOLE_HISTORY + 5 {
            console.add_history(&format!("GET {i}"));
        }
        assert_eq!(console.history().len(), CONSOLE_HISTORY);
        assert_eq!(
            console.history().last().unwrap(),
            &format!("GET {}", CONSOLE_HISTORY + 4)
        );
    }
}
