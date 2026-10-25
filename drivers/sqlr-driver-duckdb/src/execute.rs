//! Incremental native DuckDB results, bounded by the requested chunk size.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;

use duckdb::Rows;

use sqlr_driver_sdk::{
    ChunkBuilder, ColumnMeta, DriverError, ErrorKind, Execution, ResultChunk, RowStream, Value,
};
use tokio::sync::oneshot;

use crate::map_error;
use crate::types;
use crate::worker::{Command, Running, Worker, closed};

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
        Started::Rows { stream, columns } => Ok(Execution::Rows(Box::new(DuckdbRowStream {
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

/// Runs on the worker thread: starts `sql`, then serves its fetches. Other commands are
/// handled in between; a new `Execute` ends this result and is returned to run next.
pub(crate) fn run(
    conn: &duckdb::Connection,
    rx: &mpsc::Receiver<Command>,
    running: &Mutex<bool>,
    stream: u64,
    sql: &str,
    params: &[Value],
    reply: oneshot::Sender<Result<Started, DriverError>>,
) -> Ended {
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) => {
            let _ = reply.send(Err(map_error(e)));
            return DONE;
        }
    };
    // duckdb-rs 1.20000 cannot decode top-level Arrow Time64(ns) rows. Reject the
    // result before execution, including DML RETURNING, so its decoder cannot panic.
    if (0..stmt.column_count())
        .any(|index| stmt.column_logical_type(index).id() == duckdb::core::LogicalTypeId::TimeNs)
    {
        let _ = reply.send(Err(DriverError::new(
            ErrorKind::Unsupported,
            "DuckDB TIME_NS results are not supported by the native row decoder",
        )
        .with_hint(
            "Cast TIME_NS columns to VARCHAR to preserve all nine fractional digits.",
        )));
        return DONE;
    }
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
    let bound: Vec<_> = params.iter().map(types::bind).collect();
    let syntax = sqlr_driver_sdk::SqlSyntax::ansi();
    let class = syntax.classify(sql);
    let mut tokens = Vec::new();
    syntax.lex(sql, sqlr_driver_sdk::LexState::Normal, &mut tokens);
    let returning = tokens.iter().any(|token| {
        matches!(
            token.kind,
            sqlr_driver_sdk::TokenKind::Keyword | sqlr_driver_sdk::TokenKind::Ident
        ) && sql[token.start..token.end].eq_ignore_ascii_case("RETURNING")
    });
    // Start the native streaming result, then use the fallible row API to fetch it.
    // Arrow's Iterator interface would panic on fetch errors such as cancellation.
    let started = {
        let _running = Running::start(running);
        stmt.stream_arrow(duckdb::params_from_iter(bound.iter()))
            .map(|_| ())
            .map_err(map_error)
    };
    if let Err(error) = started {
        let _ = reply.send(Err(error));
        return DONE;
    }
    let columns = types::columns(&stmt);
    let mut rows = stmt.raw_query();
    let call = tokens
        .iter()
        .find(|token| token.kind != sqlr_driver_sdk::TokenKind::Comment)
        .is_some_and(|token| sql[token.start..token.end].eq_ignore_ascii_case("CALL"));
    let status_statement = matches!(
        class,
        sqlr_driver_sdk::StatementClass::Write
            | sqlr_driver_sdk::StatementClass::Ddl
            | sqlr_driver_sdk::StatementClass::Destructive(_)
            | sqlr_driver_sdk::StatementClass::Transaction(_)
    );
    // Native statements with no user result expose a single Count or Success column.
    // Inspect executed metadata before routing so FROM, SUMMARIZE, PIVOT and CALL
    // retain their result sets even when the safety classifier considers them writes.
    let status_column = columns.len() == 1
        && matches!(
            (columns[0].name.as_str(), columns[0].type_name.as_str()),
            ("Count", "BIGINT") | ("Success", "BOOLEAN")
        );
    if status_statement && status_column && !returning && !call {
        let result = {
            let _running = Running::start(running);
            rows.next().map_err(map_error).and_then(|row| match row {
                Some(row) if columns[0].name == "Count" => {
                    let count: i64 = row.get(0).map_err(map_error)?;
                    u64::try_from(count).map(Started::Affected).map_err(|_| {
                        DriverError::new(ErrorKind::Protocol, "negative affected-row count")
                    })
                }
                _ => Ok(Started::Affected(0)),
            })
        };
        let _ = reply.send(result);
        return DONE;
    }
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
            Command::Fetch { reply, .. } => {
                let _ = reply.send(Ok(None));
            }
            Command::Close { stream: s } if s == stream => return DONE,
            Command::Close { .. } => {}
            Command::Run(job) => {
                // DuckDB forbids another query on a connection with a live result.
                // A short-lived sibling reads committed metadata without disturbing it.
                match conn.try_clone() {
                    Ok(metadata) => job(&metadata),
                    Err(_) => job(conn),
                }
            }
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
                scratch.clear();
                for (i, column) in columns.iter().enumerate() {
                    // Arrow's Null carrier has no validity bitmap; its executed
                    // schema identifies it before the upstream row decoder runs.
                    scratch.push(if column.type_name == "NULL" {
                        Value::Null
                    } else {
                        types::decode(row.get_ref(i).map_err(map_error)?, column)?
                    });
                }
                let cells: Vec<_> = scratch.iter().map(Value::as_cell).collect();
                builder
                    .push_row(&cells)
                    .map_err(|error| DriverError::new(ErrorKind::Protocol, error.to_string()))?;
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
struct DuckdbRowStream {
    tx: mpsc::Sender<Command>,
    stream: u64,
    columns: Arc<[ColumnMeta]>,
    closed: bool,
}

#[async_trait::async_trait]
impl RowStream for DuckdbRowStream {
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

impl Drop for DuckdbRowStream {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.tx.send(Command::Close {
                stream: self.stream,
            });
        }
    }
}
