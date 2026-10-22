//! Query execution: results streamed row by row off the wire, row counts, warnings.
//!
//! The MySQL protocol cannot abandon a result: the rest of it must be read before the next
//! statement. A stream closed early first stops the statement with `KILL QUERY` (from the
//! control connection), then reads what is left.

use std::sync::{Arc, Mutex as StdMutex};

use mysql_async::prelude::Queryable;
use mysql_async::{BinaryProtocol, Column, Conn, Params, QueryResult, Row, TextProtocol};
use sqlr_core::{
    ChunkBuilder, ColumnMeta, DriverError, ErrorKind, Execution, ResultChunk, RowStream,
    trim_statement_in,
};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::connect::Killer;
use crate::map_error;
use crate::syntax::mysql_syntax;
use crate::types::{self, Decode, Decoded};

/// Server warnings and notes not yet taken.
pub(crate) type Notices = Arc<StdMutex<Vec<String>>>;

/// A statement to run: plain text, or prepared with bound parameters.
pub(crate) enum Statement<'a> {
    Text(&'a str),
    Prepared(&'a str, Vec<mysql_async::Value>),
}

/// A result still on the wire of the query connection.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pending {
    /// Read with the binary protocol (a prepared statement).
    binary: bool,
    /// Rows of the streamed result set remain unread.
    rows_left: bool,
}

/// The connection that runs user queries; a live result stream holds its lock.
pub(crate) struct QueryConn {
    pub(crate) conn: Conn,
    pending: Option<Pending>,
}

impl QueryConn {
    pub(crate) fn new(conn: Conn) -> Self {
        Self {
            conn,
            pending: None,
        }
    }

    /// The next row of the current result set; `None` at its end.
    async fn next_row(&mut self, binary: bool) -> mysql_async::Result<Option<Row>> {
        if binary {
            QueryResult::<'_, '_, BinaryProtocol>::new(&mut self.conn)
                .next()
                .await
        } else {
            QueryResult::<'_, '_, TextProtocol>::new(&mut self.conn)
                .next()
                .await
        }
    }

    /// Whether more result sets follow (a procedure call, several statements).
    fn more_results(&mut self, binary: bool) -> bool {
        if binary {
            !QueryResult::<'_, '_, BinaryProtocol>::new(&mut self.conn).is_empty()
        } else {
            !QueryResult::<'_, '_, TextProtocol>::new(&mut self.conn).is_empty()
        }
    }

    /// Reads and drops whatever is left of the last statement's results.
    async fn drain(&mut self, binary: bool) -> mysql_async::Result<()> {
        if binary {
            QueryResult::<'_, '_, BinaryProtocol>::new(&mut self.conn)
                .drop_result()
                .await
        } else {
            QueryResult::<'_, '_, TextProtocol>::new(&mut self.conn)
                .drop_result()
                .await
        }
    }

    /// Ends the previous statement: stops it when rows were left unread, then drains it.
    /// Errors belong to that statement (most often "query execution was interrupted") and
    /// are only logged.
    async fn finish(&mut self, killer: &Killer) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        if pending.rows_left
            && let Err(e) = killer.kill_query(self.conn.id()).await
        {
            tracing::warn!("could not stop the unfinished MySQL query: {e}");
        }
        if let Err(e) = self.drain(pending.binary).await {
            tracing::debug!("draining an unfinished MySQL result: {e}");
        }
    }
}

/// Appends the server's `SHOW WARNINGS` to `notices`.
async fn collect_warnings(conn: &mut Conn, notices: &Notices) {
    match conn.query::<Row, _>("SHOW WARNINGS").await {
        Ok(rows) => {
            let mut list = notices.lock().unwrap_or_else(|e| e.into_inner());
            for row in rows {
                let field = |i| match row.as_ref(i) {
                    Some(mysql_async::Value::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
                    Some(mysql_async::Value::NULL) | None => String::new(),
                    Some(v) => v.as_sql(true),
                };
                list.push(format!("{} {}: {}", field(0), field(1), field(2)));
            }
        }
        Err(e) => tracing::warn!("SHOW WARNINGS failed: {e}"),
    }
}

pub(crate) async fn execute(
    query: Arc<Mutex<QueryConn>>,
    killer: Arc<Killer>,
    notices: Notices,
    statement: Statement<'_>,
) -> Result<Execution, DriverError> {
    let mut guard = query.lock_owned().await;
    guard.finish(&killer).await;
    let (columns, binary) = match statement {
        Statement::Text(sql) => {
            let result = guard
                .conn
                .query_iter(trim_statement_in(&mysql_syntax(), sql))
                .await
                .map_err(map_error)?;
            (result.columns(), false)
        }
        Statement::Prepared(sql, params) => {
            let params = if params.is_empty() {
                Params::Empty
            } else {
                Params::Positional(params)
            };
            let result = guard
                .conn
                .exec_iter(trim_statement_in(&mysql_syntax(), sql), params)
                .await
                .map_err(map_error)?;
            (result.columns(), true)
        }
    };

    let Some(raw) = columns.filter(|c| !c.is_empty()) else {
        // An OK packet. Its counts are read before draining any further results.
        let affected = guard.conn.affected_rows();
        let warnings = guard.conn.get_warnings();
        guard.drain(binary).await.map_err(map_error)?;
        if warnings > 0 {
            collect_warnings(&mut guard.conn, &notices).await;
        }
        return Ok(Execution::Affected(affected));
    };

    let (metas, decoders): (Vec<ColumnMeta>, Vec<Decode>) = raw.iter().map(types::column).unzip();
    guard.pending = Some(Pending {
        binary,
        rows_left: true,
    });
    Ok(Execution::Rows(Box::new(MyRowStream {
        guard: Some(guard),
        columns: metas.into(),
        decoders,
        raw,
        binary,
        done: false,
        notices,
        killer,
    })))
}

/// Holds the query connection until it is closed or dropped; the next `execute` waits for
/// that, then finishes whatever is left on the wire.
struct MyRowStream {
    /// `None` once closed.
    guard: Option<OwnedMutexGuard<QueryConn>>,
    columns: Arc<[ColumnMeta]>,
    decoders: Vec<Decode>,
    /// Kept for the row width check.
    raw: Arc<[Column]>,
    binary: bool,
    /// The result set ended (or failed).
    done: bool,
    notices: Notices,
    killer: Arc<Killer>,
}

impl MyRowStream {
    /// After the last row: drops further result sets and collects warnings.
    async fn end(&mut self) -> Result<(), DriverError> {
        self.done = true;
        let Some(guard) = self.guard.as_mut() else {
            return Ok(());
        };
        let more = guard.more_results(self.binary);
        let warnings = guard.conn.get_warnings();
        guard.pending = None;
        if more {
            guard.drain(self.binary).await.map_err(map_error)?;
        }
        if warnings > 0 {
            collect_warnings(&mut guard.conn, &self.notices).await;
        }
        Ok(())
    }
}

fn protocol(message: impl std::fmt::Display) -> DriverError {
    DriverError::new(ErrorKind::Protocol, message.to_string())
}

#[async_trait::async_trait]
impl RowStream for MyRowStream {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }

    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        if self.done {
            return Ok(None);
        }
        let Some(guard) = self.guard.as_mut() else {
            return Ok(None);
        };
        let mut builder = ChunkBuilder::new(self.columns.clone());
        let mut ended = false;
        while builder.len() < max_rows.max(1) {
            let row = match guard.next_row(self.binary).await {
                Ok(Some(row)) => row,
                Ok(None) => {
                    ended = true;
                    break;
                }
                Err(e) => {
                    // An error packet ends the result; nothing is left on the wire.
                    self.done = true;
                    guard.pending = None;
                    return Err(map_error(e));
                }
            };
            if row.len() != self.raw.len() {
                return Err(protocol(format!(
                    "row has {} values, expected {}",
                    row.len(),
                    self.raw.len()
                )));
            }
            let mut decoded: Vec<Decoded<'_>> = Vec::with_capacity(self.decoders.len());
            for (i, decoder) in self.decoders.iter().enumerate() {
                let value = row
                    .as_ref(i)
                    .ok_or_else(|| protocol("row value already taken"))?;
                decoded.push(types::decode(*decoder, value)?);
            }
            let cells: Vec<_> = decoded.iter().map(Decoded::cell).collect();
            builder.push_row(&cells).map_err(protocol)?;
        }
        let empty = builder.is_empty();
        let chunk = builder.finish();
        if ended {
            self.end().await?;
        }
        Ok((!empty).then_some(chunk))
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        self.done = true;
        // Releasing the guard lets the next `execute` run even if this stream is kept alive.
        if let Some(mut guard) = self.guard.take() {
            guard.finish(&self.killer).await;
        }
        Ok(())
    }
}
