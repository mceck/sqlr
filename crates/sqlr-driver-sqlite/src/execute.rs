//! Statements: row counts, and results stepped incrementally on the worker thread.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use rusqlite::Rows;
use rusqlite::types::ValueRef;
use sqlr_core::{
    ChunkBuilder, ColumnMeta, DriverError, ErrorKind, Execution, ResultChunk, RowStream, Value,
};
use tokio::sync::oneshot;

use crate::map_error;
use crate::types::{self, column_metas, declared};
use crate::worker::{Command, Worker, closed, cut_error};

pub(crate) enum Started {
    Rows {
        stream: u64,
        columns: Arc<[ColumnMeta]>,
    },
    Affected(u64),
}

pub(crate) async fn execute(
    worker: &Worker,
    sql: String,
    params: Vec<Value>,
) -> Result<Execution, DriverError> {
    let (reply, started) = oneshot::channel();
    worker.send(Command::Execute { sql, params, reply })?;
    match started.await.map_err(|_| closed())?? {
        Started::Affected(n) => Ok(Execution::Affected(n)),
        Started::Rows { stream, columns } => Ok(Execution::Rows(Box::new(SqliteRowStream {
            tx: worker.sender(),
            stream,
            columns,
            closed: false,
        }))),
    }
}

/// How a statement's turn on the worker ended.
pub(crate) struct Ended {
    /// A newer `Execute` that ended this result, to run next.
    pub(crate) next: Option<Command>,
    /// The result was ended before it was read to the end.
    pub(crate) cut: bool,
}

const DONE: Ended = Ended {
    next: None,
    cut: false,
};

/// Marks a user statement as stepping for as long as it lives.
struct Running<'a>(&'a AtomicBool);

impl<'a> Running<'a> {
    fn start(flag: &'a AtomicBool) -> Self {
        flag.store(true, Ordering::SeqCst);
        Self(flag)
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Runs on the worker thread: starts `sql`, then serves its fetches. Other commands are
/// handled in between; a new `Execute` ends this result and is returned to run next.
#[expect(
    clippy::too_many_arguments,
    reason = "the worker's statement context: connection, channel, run state, stream, SQL, parameters, reply and the cut results"
)]
pub(crate) fn run(
    conn: &rusqlite::Connection,
    rx: &mpsc::Receiver<Command>,
    running: &AtomicBool,
    stream: u64,
    sql: &str,
    params: &[Value],
    reply: oneshot::Sender<Result<Started, DriverError>>,
    cut: &HashSet<u64>,
) -> Ended {
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) => {
            let _ = reply.send(Err(map_error(e)));
            return DONE;
        }
    };
    if stmt.parameter_count() != params.len() {
        let _ = reply.send(Err(DriverError::new(
            ErrorKind::Other,
            format!(
                "the statement takes {} parameters, {} were given",
                stmt.parameter_count(),
                params.len()
            ),
        )));
        return DONE;
    }
    for (i, param) in params.iter().enumerate() {
        if let Err(e) = stmt.raw_bind_parameter(i + 1, types::to_sql(param)) {
            let _ = reply.send(Err(map_error(e)));
            return DONE;
        }
    }
    if stmt.column_count() == 0 {
        let before = conn.total_changes();
        let _running = Running::start(running);
        let result = stmt.raw_execute().map(|_| {
            // DDL leaves `changes()` at the last DML's count.
            if conn.total_changes() == before {
                0
            } else {
                conn.changes()
            }
        });
        let _ = reply.send(result.map(Started::Affected).map_err(map_error));
        return DONE;
    }

    let columns = column_metas(declared(&stmt));
    let mut rows = stmt.raw_query();
    if reply
        .send(Ok(Started::Rows {
            stream,
            columns: columns.clone(),
        }))
        .is_err()
    {
        return DONE;
    }

    let mut exhausted = false;
    loop {
        let Ok(command) = rx.recv() else {
            return DONE;
        };
        match command {
            Command::Fetch {
                stream: s,
                max_rows,
                reply,
            } if s == stream => {
                let result = if exhausted {
                    Ok(None)
                } else {
                    let _running = Running::start(running);
                    fetch(&mut rows, &columns, max_rows.max(1), &mut exhausted)
                };
                let _ = reply.send(result);
            }
            Command::Fetch {
                stream: s, reply, ..
            } => {
                let _ = reply.send(if cut.contains(&s) {
                    Err(cut_error())
                } else {
                    Ok(None)
                });
            }
            Command::Close { stream: s } if s == stream => return DONE,
            Command::Close { .. } => {}
            Command::Run(job) => job(conn),
            command @ Command::Execute { .. } => {
                return Ended {
                    next: Some(command),
                    cut: !exhausted,
                };
            }
        }
    }
}

fn fetch(
    rows: &mut Rows<'_>,
    columns: &Arc<[ColumnMeta]>,
    max_rows: usize,
    exhausted: &mut bool,
) -> Result<Option<ResultChunk>, DriverError> {
    let mut builder = ChunkBuilder::new(columns.clone());
    let mut scratch = Vec::new();
    while builder.len() < max_rows {
        match rows.next() {
            Ok(Some(row)) => {
                let refs: Vec<ValueRef<'_>> =
                    (0..columns.len()).map(|i| row.get_ref_unwrap(i)).collect();
                types::push_row(&mut builder, &refs, &mut scratch)?;
            }
            Ok(None) => {
                *exhausted = true;
                break;
            }
            Err(e) => {
                *exhausted = true;
                return Err(map_error(e));
            }
        }
    }
    Ok((!builder.is_empty()).then(|| builder.finish()))
}

/// A result being stepped on the worker thread. Dropping it ends the result there.
struct SqliteRowStream {
    tx: mpsc::Sender<Command>,
    stream: u64,
    columns: Arc<[ColumnMeta]>,
    closed: bool,
}

#[async_trait::async_trait]
impl RowStream for SqliteRowStream {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }

    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        if self.closed {
            return Ok(None);
        }
        let (reply, chunk) = oneshot::channel();
        self.tx
            .send(Command::Fetch {
                stream: self.stream,
                max_rows,
                reply,
            })
            .map_err(|_| closed())?;
        chunk.await.map_err(|_| closed())?
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        if !self.closed {
            self.closed = true;
            let _ = self.tx.send(Command::Close {
                stream: self.stream,
            });
        }
        Ok(())
    }
}

impl Drop for SqliteRowStream {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.tx.send(Command::Close {
                stream: self.stream,
            });
        }
    }
}
