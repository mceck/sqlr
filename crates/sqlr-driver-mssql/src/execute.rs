//! Query execution: demand-driven, bounded TDS streaming and row counts.

use std::sync::Arc;

use futures_util::TryStreamExt;
use sqlr_core::{
    Cell, ChunkBuilder, ColumnMeta, Dialect, DriverError, ErrorKind, Execution, LexState,
    ResultChunk, RowStream, TokenKind, Value, trim_statement_in,
};
use tiberius::{ColumnData, QueryItem, ToSql};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::browse::MsClient;
use crate::dialect::MsDialect;
use crate::map_error;
use crate::syntax::tsql_syntax;
use crate::types;

pub(crate) async fn execute(
    client: Arc<Mutex<MsClient>>,
    sql: &str,
    params: &[Value],
) -> Result<Execution, DriverError> {
    let syntax = tsql_syntax();
    let sql = statement_text(sql);
    let class = MsDialect
        .statement_class(&sql)
        .unwrap_or_else(|| syntax.classify(&sql));
    // `CREATE VIEW`, `CREATE SCHEMA` and the like must be the first statement of a batch, and
    // a `SET SHOWPLAN_…` only holds for the session when it is one: a statement run with
    // `execute` travels inside `sp_executesql`. Transaction commands must also be plain
    // batches: changing @@TRANCOUNT inside that procedure raises error 266 even though
    // the command took effect. These statements have no row count to report.
    if params.is_empty()
        && (syntax.is_ddl(&sql)
            || first_word(&sql).eq_ignore_ascii_case("SET")
            || matches!(class, sqlr_core::StatementClass::Transaction(_)))
    {
        client
            .lock()
            .await
            .simple_query(sql.as_ref())
            .await
            .map_err(map_error)?
            .into_results()
            .await
            .map_err(map_error)?;
        return Ok(Execution::Affected(0));
    }
    if !returns_rows(&sql) {
        let encoded: Vec<ColumnData<'static>> = params.iter().map(types::encode).collect();
        let refs: Vec<&dyn ToSql> = encoded.iter().map(|v| v as &dyn ToSql).collect();
        let result = client
            .lock()
            .await
            .execute(sql.as_ref(), &refs)
            .await
            .map_err(map_error)?;
        return Ok(Execution::Affected(result.total()));
    }

    let (send, receive) = mpsc::channel(1);
    let (initial, metadata) = oneshot::channel();
    let sql = sql.into_owned();
    let params = params.to_vec();
    // The task owns the client guard; its borrowing QueryStream never leaves it. A
    // request is required for every chunk, so an idle viewer exerts backpressure.
    tokio::spawn(async move {
        let mut client = client.lock_owned().await;
        let cleanup = serve_rows(&mut client, &sql, &params, initial, receive).await;
        if let Some(reply) = cleanup.close {
            let _ = reply.send(cleanup.result);
        }
    });
    let Some(columns) = metadata.await.map_err(|_| stopped())?? else {
        return Ok(Execution::Affected(0));
    };
    Ok(Execution::Rows(Box::new(MsRows {
        columns,
        requests: Some(send),
    })))
}

type PageReply = oneshot::Sender<Result<Option<ResultChunk>, DriverError>>;
enum Request {
    Fetch(usize, PageReply),
    Close(oneshot::Sender<Result<(), DriverError>>),
}
struct Cleanup {
    result: Result<(), DriverError>,
    close: Option<oneshot::Sender<Result<(), DriverError>>>,
}

async fn serve_rows(
    client: &mut MsClient,
    sql: &str,
    params: &[Value],
    mut initial: oneshot::Sender<Result<Option<Arc<[ColumnMeta]>>, DriverError>>,
    mut requests: mpsc::Receiver<Request>,
) -> Cleanup {
    let encoded: Vec<ColumnData<'static>> = params.iter().map(types::encode).collect();
    let refs: Vec<&dyn ToSql> = encoded.iter().map(|v| v as &dyn ToSql).collect();
    if initial.is_closed() {
        return Cleanup {
            result: Ok(()),
            close: None,
        };
    }
    let mut stream = match client.query(sql, &refs).await {
        Ok(stream) => stream,
        Err(e) => {
            let _ = initial.send(Err(map_error(e)));
            return Cleanup {
                result: Ok(()),
                close: None,
            };
        }
    };
    let columns = loop {
        let next = tokio::select! {
            _ = initial.closed() => return Cleanup {result:Ok(()),close:None},
            item = stream.try_next() => item,
        };
        match next {
            Ok(Some(QueryItem::Metadata(meta))) if meta.result_index() == 0 => {
                break meta
                    .columns()
                    .iter()
                    .map(types::column_meta)
                    .collect::<Vec<_>>()
                    .into();
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                let _ = initial.send(Ok(None));
                return Cleanup {
                    result: Ok(()),
                    close: None,
                };
            }
            Err(e) => {
                let _ = initial.send(Err(map_error(e)));
                return Cleanup {
                    result: Ok(()),
                    close: None,
                };
            }
        }
    };
    let columns: Arc<[ColumnMeta]> = columns;
    if initial.send(Ok(Some(columns.clone()))).is_err() {
        return Cleanup {
            result: Ok(()),
            close: None,
        };
    }
    while let Some(request) = requests.recv().await {
        match request {
            Request::Close(reply) => {
                // Draining is bounded and preserves all side effects in a batch. Explicit
                // query cancellation still uses the connection's existing cancel handle.
                // Tiberius Attention cannot reliably resynchronise a partial RPC response.
                let result = async {
                    while stream.try_next().await.map_err(map_error)?.is_some() {}
                    Ok(())
                }
                .await;
                return Cleanup {
                    result,
                    close: Some(reply),
                };
            }
            Request::Fetch(max, mut reply) => {
                let mut builder = ChunkBuilder::new(columns.clone());
                let mut exhausted = false;
                let page = async {
                    while builder.len() < max.max(1) {
                        match stream.try_next().await.map_err(map_error)? {
                            Some(QueryItem::Row(row)) if row.result_index() == 0 => {
                                let values: Vec<_> = row
                                    .cells()
                                    .zip(columns.iter())
                                    .map(|((_, data), meta)| {
                                        types::fit(types::decode(data), meta.kind)
                                    })
                                    .collect();
                                let cells: Vec<Cell<'_>> =
                                    values.iter().map(Value::as_cell).collect();
                                builder.push_row(&cells).map_err(|e| {
                                    DriverError::new(ErrorKind::Protocol, e.to_string())
                                })?;
                            }
                            Some(_) => {}
                            None => {
                                exhausted = true;
                                break;
                            }
                        }
                    }
                    Ok(if builder.is_empty() {
                        None
                    } else {
                        Some(builder.finish())
                    })
                };
                let result = tokio::select! {
                    _ = reply.closed() => return Cleanup {result:Ok(()),close:None},
                    result = page => result,
                };
                let failed = result.is_err();
                if reply.send(result).is_err() || failed {
                    return Cleanup {
                        result: Ok(()),
                        close: None,
                    };
                }
                if exhausted {
                    return Cleanup {
                        result: Ok(()),
                        close: None,
                    };
                }
            }
        }
    }
    // The caller dropped its result without calling close.
    Cleanup {
        result: Ok(()),
        close: None,
    }
}

fn stopped() -> DriverError {
    DriverError::new(ErrorKind::Connection, "SQL Server result worker stopped")
}

/// `sql` trimmed, with the `;` T-SQL requires at the end of a `MERGE`, which trimming just
/// took off. The terminator goes on a line of its own, so a trailing line comment cannot
/// swallow it.
fn statement_text(sql: &str) -> std::borrow::Cow<'_, str> {
    let sql = trim_statement_in(&tsql_syntax(), sql);
    if first_word(&sql).eq_ignore_ascii_case("MERGE") {
        std::borrow::Cow::Owned(format!("{sql}\n;"))
    } else {
        sql
    }
}

/// The rows `spec.sql` returns between `spec.before` and `spec.after`, all as plain
/// batches: a showplan comes back as a rowset only for a batch of its own. `after` runs
/// whatever happened.
pub(crate) async fn explain_rows(
    client: &mut MsClient,
    spec: &sqlr_core::ExplainSpec,
) -> Result<Vec<Vec<Value>>, DriverError> {
    async fn batch(client: &mut MsClient, sql: &str) -> Result<Vec<tiberius::Row>, DriverError> {
        client
            .simple_query(sql)
            .await
            .map_err(map_error)?
            .into_first_result()
            .await
            .map_err(map_error)
    }
    let mut result = Ok(Vec::new());
    for sql in &spec.before {
        if let Err(e) = batch(client, sql).await {
            result = Err(e);
            break;
        }
    }
    if result.is_ok() {
        result = batch(client, &spec.sql).await;
    }
    for sql in &spec.after {
        let _ = batch(client, sql).await;
    }
    Ok(result?
        .iter()
        .map(|row| row.cells().map(|(_, data)| types::decode(data)).collect())
        .collect())
}

/// The first word of `sql`, comments skipped.
fn first_word(sql: &str) -> String {
    let mut tokens = Vec::new();
    tsql_syntax().lex(sql, LexState::Normal, &mut tokens);
    tokens
        .iter()
        .find(|t| t.kind != TokenKind::Comment)
        .map(|t| sql[t.start..t.end].to_string())
        .unwrap_or_default()
}

/// Whether a statement produces a rowset rather than only a row count.
fn returns_rows(sql: &str) -> bool {
    let mut tokens = Vec::new();
    tsql_syntax().lex(sql, LexState::Normal, &mut tokens);
    let first = tokens
        .iter()
        .find(|t| t.kind != TokenKind::Comment)
        .map(|t| sql[t.start..t.end].to_ascii_uppercase())
        .unwrap_or_default();
    if matches!(
        first.as_str(),
        "SELECT" | "EXEC" | "EXECUTE" | "DECLARE" | "PRINT" | "DBCC" | "VALUES"
    ) {
        return true;
    }
    // A CTE can feed an UPDATE, INSERT, DELETE or MERGE, not only a SELECT. Sending
    // those through `query` loses the DONE row counts when no OUTPUT rowset follows.
    if first == "WITH" && tsql_syntax().classify(sql) == sqlr_core::StatementClass::Read {
        return true;
    }
    // `UPDATE … OUTPUT`, `INSERT … OUTPUT` and `DELETE … OUTPUT` return rows. Only a
    // keyword counts: the text " OUTPUT " inside a string or a comment does not.
    tokens.iter().any(|t| {
        matches!(t.kind, TokenKind::Keyword | TokenKind::Ident)
            && sql[t.start..t.end].eq_ignore_ascii_case("OUTPUT")
    })
}

/// A handle to the demand-driven worker; at most one page is in flight.
struct MsRows {
    columns: Arc<[ColumnMeta]>,
    requests: Option<mpsc::Sender<Request>>,
}

#[async_trait::async_trait]
impl RowStream for MsRows {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }
    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        let Some(requests) = &self.requests else {
            return Ok(None);
        };
        let (reply, receive) = oneshot::channel();
        if requests
            .send(Request::Fetch(max_rows, reply))
            .await
            .is_err()
        {
            self.requests = None;
            return Ok(None);
        }
        receive.await.map_err(|_| stopped())?
    }
    async fn close(&mut self) -> Result<(), DriverError> {
        if let Some(requests) = self.requests.take() {
            let (reply, receive) = oneshot::channel();
            if requests.send(Request::Close(reply)).await.is_ok() {
                return receive.await.map_err(|_| stopped())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{returns_rows, statement_text};

    #[test]
    fn a_merge_terminator_is_not_swallowed_by_a_trailing_comment() {
        let sql = statement_text(
            "MERGE INTO t USING s ON t.id = s.id \
             WHEN NOT MATCHED THEN INSERT (id) VALUES (s.id) -- upsert",
        );
        assert!(sql.ends_with("\n;"), "{sql}");
        assert!(!sql.contains("-- upsert;"), "{sql}");
        // A MERGE that already ends in `;` gets exactly one.
        let sql = statement_text(
            "MERGE INTO t USING s ON 1 = 1 WHEN NOT MATCHED THEN INSERT (a) VALUES (1);",
        );
        assert_eq!(sql.matches(';').count(), 1, "{sql}");
        // Other statements lose only their terminator.
        assert_eq!(statement_text("SELECT 1;"), "SELECT 1");
    }

    #[test]
    fn only_rowsets_are_queried() {
        assert!(returns_rows("SELECT 1"));
        assert!(returns_rows(
            "  -- note\n WITH x AS (SELECT 1) SELECT * FROM x"
        ));
        assert!(returns_rows("INSERT INTO t OUTPUT inserted.id VALUES (1)"));
        assert!(!returns_rows("INSERT INTO t VALUES (1)"));
        assert!(!returns_rows("UPDATE t SET a = 1"));
        // The word inside data is not an OUTPUT clause: querying would lose the count.
        assert!(!returns_rows("UPDATE t SET note = 'a OUTPUT b'"));
        assert!(!returns_rows("UPDATE t SET note = 'x' -- OUTPUT\n"));
        assert!(returns_rows("UPDATE t SET a = 1 OUTPUT inserted.a"));
    }
}
