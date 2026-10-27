//! The thread that owns one `rusqlite::Connection`. The async side sends it commands over a
//! channel; an open result keeps its statement on this thread between fetches.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;

use rusqlite::{InterruptHandle, OpenFlags};
use sqlr_core::{DriverError, ErrorKind, ResultChunk, Value};
use tokio::sync::oneshot;

use crate::execute::{self, Started};
use crate::map_error;

pub(crate) type Job = Box<dyn FnOnce(&rusqlite::Connection) + Send>;

pub(crate) enum Command {
    /// Prepares and starts a statement; a running result is ended first.
    Execute {
        sql: String,
        params: Vec<Value>,
        reply: oneshot::Sender<Result<Started, DriverError>>,
    },
    /// The next chunk of result `stream`; `None` once it is exhausted or was ended.
    Fetch {
        stream: u64,
        max_rows: usize,
        reply: oneshot::Sender<Result<Option<ResultChunk>, DriverError>>,
    },
    Close {
        stream: u64,
    },
    /// Any other work on the connection (explorer, catalog, ping).
    Run(Job),
}

pub(crate) fn closed() -> DriverError {
    DriverError::new(ErrorKind::Connection, "the SQLite connection is closed")
}

pub(crate) struct Worker {
    tx: mpsc::Sender<Command>,
    interrupt: InterruptHandle,
    /// Set while a user statement steps (not while explorer or catalog work runs), so a
    /// cancel never interrupts anything else.
    running: Arc<AtomicBool>,
    /// Stops a long administration read (row counts): see [`Worker::stop_admin`].
    pub(crate) admin: Arc<AdminStop>,
}

/// The state an administration read checks to stop early. Each read has its own
/// generation, so a stop only ever reaches the read it was given for: one that arrives late
/// (its read already ended) stops nothing.
#[derive(Default)]
pub(crate) struct AdminStop {
    /// The last generation handed out.
    next: AtomicU64,
    /// The generation of the latest stop handle not yet claimed by a read.
    issued: AtomicU64,
    /// The generation a stop was asked for.
    stopped: AtomicU64,
    /// The generation of the read running on the connection's thread (0 when none). The
    /// lock also keeps a read from ending between a stop's check and its interrupt.
    running: parking_lot::Mutex<u64>,
}

impl AdminStop {
    /// A new generation for a stop handle; the next read claims it.
    pub(crate) fn issue(&self) -> u64 {
        let generation = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        self.issued.store(generation, Ordering::SeqCst);
        generation
    }

    /// The generation of a read about to be queued: the latest unclaimed handle's, else a
    /// fresh one no handle stops.
    pub(crate) fn claim(&self) -> u64 {
        match self.issued.swap(0, Ordering::SeqCst) {
            0 => self.next.fetch_add(1, Ordering::SeqCst) + 1,
            generation => generation,
        }
    }

    /// Whether the read of `generation` was asked to stop.
    pub(crate) fn is_stopped(&self, generation: u64) -> bool {
        self.stopped.load(Ordering::SeqCst) == generation
    }

    /// The read of `generation` starts (`true`) or ends (`false`) on the connection's thread.
    pub(crate) fn set_running(&self, generation: u64, running: bool) {
        *self.running.lock() = if running { generation } else { 0 };
    }
}

impl Worker {
    /// Opens `path` on a new thread; fails when the file cannot be opened or is not a
    /// database.
    pub(crate) async fn open(path: PathBuf, flags: OpenFlags) -> Result<Self, DriverError> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let running = Arc::new(AtomicBool::new(false));
        let flag = running.clone();
        std::thread::Builder::new()
            .name("sqlr-sqlite".into())
            .spawn(move || {
                let opened = rusqlite::Connection::open_with_flags(&path, flags)
                    .and_then(|conn| {
                        // SQLite reads the file lazily; this makes a non-database fail now.
                        conn.query_row("PRAGMA schema_version", [], |_| Ok(()))?;
                        Ok(conn)
                    })
                    .map_err(map_error);
                match opened {
                    Ok(conn) => {
                        if ready_tx.send(Ok(conn.get_interrupt_handle())).is_ok() {
                            run(&conn, &rx, &flag);
                        }
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| DriverError::new(ErrorKind::Other, format!("cannot start SQLite: {e}")))?;
        let interrupt = ready_rx.await.map_err(|_| closed())??;
        Ok(Self {
            tx,
            interrupt,
            running,
            admin: Arc::default(),
        })
    }

    /// Stops the administration read of `generation`, running or still queued: it sees the
    /// stop between tables, and a statement it runs is interrupted. Any other read, and
    /// whatever runs after it, is left alone.
    pub(crate) fn stop_admin(&self, generation: u64) {
        self.admin.stopped.store(generation, Ordering::SeqCst);
        let running = self.admin.running.lock();
        if *running == generation {
            self.interrupt.interrupt();
        }
    }

    pub(crate) fn send(&self, command: Command) -> Result<(), DriverError> {
        self.tx.send(command).map_err(|_| closed())
    }

    pub(crate) fn sender(&self) -> mpsc::Sender<Command> {
        self.tx.clone()
    }

    /// Runs `f` on the connection's thread.
    pub(crate) async fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> R + Send + 'static,
    ) -> Result<R, DriverError> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Run(Box::new(move |conn| {
            let _ = tx.send(f(conn));
        })))?;
        rx.await.map_err(|_| closed())
    }

    /// Interrupts the user statement that is stepping, if one is. Between fetches nothing
    /// runs, and SQLite would otherwise keep the interrupt pending for the paused result (and
    /// any explorer query) until it ends.
    pub(crate) fn interrupt(&self) {
        if self.running.load(Ordering::SeqCst) {
            self.interrupt.interrupt();
        }
    }
}

/// The command loop. Ends when every sender (the connection and its streams) is gone.
fn run(conn: &rusqlite::Connection, rx: &mpsc::Receiver<Command>, running: &AtomicBool) {
    let mut next_stream = 1;
    let mut pending: Option<Command> = None;
    // Results a newer statement ended before they were read to the end.
    let mut cut: HashSet<u64> = HashSet::new();
    loop {
        let command = match pending.take() {
            Some(c) => c,
            None => match rx.recv() {
                Ok(c) => c,
                Err(_) => break,
            },
        };
        match command {
            Command::Execute { sql, params, reply } => {
                let stream = next_stream;
                next_stream += 1;
                let ended = execute::run(conn, rx, running, stream, &sql, &params, reply);
                if ended.cut {
                    cut.insert(stream);
                }
                pending = ended.next;
            }
            Command::Fetch { stream, reply, .. } => {
                let _ = reply.send(if cut.contains(&stream) {
                    Err(DriverError::new(
                        ErrorKind::Cancelled,
                        "the result was ended by a newer statement",
                    ))
                } else {
                    Ok(None)
                });
            }
            Command::Close { stream } => {
                cut.remove(&stream);
            }
            Command::Run(job) => job(conn),
        }
    }
}
