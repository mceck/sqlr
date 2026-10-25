//! One background thread owns each native connection and its live result cursor.
use crate::{
    execute::{self, Started},
    map_error,
};
use duckdb::{AccessMode, Config, InterruptHandle};
use sqlr_driver_sdk::{DriverError, ErrorKind, ResultChunk, Value};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex, mpsc},
};
use tokio::sync::oneshot;

type Job = Box<dyn FnOnce(&duckdb::Connection) + Send>;
pub(crate) enum Command {
    Execute {
        sql: String,
        params: Vec<Value>,
        reply: oneshot::Sender<Result<Started, DriverError>>,
    },
    Fetch {
        stream: u64,
        max_rows: usize,
        reply: oneshot::Sender<Result<Option<ResultChunk>, DriverError>>,
    },
    Close {
        stream: u64,
    },
    Run(Job),
}
pub(crate) fn closed() -> DriverError {
    DriverError::new(ErrorKind::Connection, "the DuckDB connection is closed")
}
pub(crate) struct Worker {
    tx: mpsc::Sender<Command>,
    interrupt: Arc<InterruptHandle>,
    running: Arc<Mutex<bool>>,
}
impl Worker {
    pub(crate) async fn open(path: String, read_only: bool) -> Result<Self, DriverError> {
        Self::start(move || {
            let mut config = Config::default()
                .enable_autoload_extension(false)
                .map_err(map_error)?;
            if read_only {
                config = config
                    .access_mode(AccessMode::ReadOnly)
                    .map_err(map_error)?;
            }
            duckdb::Connection::open_with_flags(path, config).map_err(map_error)
        })
        .await
    }
    pub(crate) async fn clone_connection(&self) -> Result<Self, DriverError> {
        let connection = self
            .call(|conn| conn.try_clone().map_err(map_error))
            .await??;
        Self::start(move || Ok(connection)).await
    }
    async fn start(
        open: impl FnOnce() -> Result<duckdb::Connection, DriverError> + Send + 'static,
    ) -> Result<Self, DriverError> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let running = Arc::new(Mutex::new(false));
        let flag = running.clone();
        std::thread::Builder::new()
            .name("sqlr-duckdb".into())
            .spawn(move || match open() {
                Ok(conn) => {
                    // Lossless Arrow transport retains large-integer bits and TIMETZ offsets.
                    if let Err(error) = conn.execute_batch("SET arrow_lossless_conversion = true") {
                        let _ = ready_tx.send(Err(map_error(error)));
                        return;
                    }
                    if ready_tx.send(Ok(conn.interrupt_handle())).is_ok() {
                        run(conn, rx, &flag);
                    }
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|error| {
                DriverError::new(
                    ErrorKind::Connection,
                    format!("cannot start DuckDB: {error}"),
                )
            })?;
        let interrupt = ready_rx.await.map_err(|_| closed())??;
        Ok(Self {
            tx,
            interrupt,
            running,
        })
    }
    pub(crate) fn send(&self, command: Command) -> Result<(), DriverError> {
        self.tx.send(command).map_err(|_| closed())
    }
    pub(crate) fn sender(&self) -> mpsc::Sender<Command> {
        self.tx.clone()
    }
    pub(crate) async fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&duckdb::Connection) -> R + Send + 'static,
    ) -> Result<R, DriverError> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Run(Box::new(move |conn| {
            let _ = tx.send(f(conn));
        })))?;
        rx.await.map_err(|_| closed())
    }
    pub(crate) fn interrupt(&self) {
        let running = self
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *running {
            self.interrupt.interrupt();
        }
    }
}
/// The lock couples the end of execution with cancellation, preventing a late interrupt
/// from leaking into catalog work or a later statement.
pub(crate) struct Running<'a>(&'a Mutex<bool>);
impl<'a> Running<'a> {
    pub(crate) fn start(flag: &'a Mutex<bool>) -> Self {
        *flag
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        Self(flag)
    }
}
impl Drop for Running<'_> {
    fn drop(&mut self) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
    }
}
fn run(conn: duckdb::Connection, rx: mpsc::Receiver<Command>, running: &Mutex<bool>) {
    let mut next_stream = 1;
    let mut pending = None;
    let mut cut = HashSet::new();
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
                let ended = execute::run(&conn, &rx, running, stream, &sql, &params, reply);
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
            Command::Run(job) => job(&conn),
        }
    }
}
