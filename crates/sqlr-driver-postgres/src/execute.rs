//! Query execution: cursor-streamed reads, bounded wire streams and row counts.

use futures_util::TryStreamExt;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use sqlr_core::{
    Cell, ChunkBuilder, ColumnKind, ColumnMeta, ColumnOrigin, DriverError, ErrorKind, Execution,
    LexState, ResultChunk, RowStream, TableRef, TokenKind, Value,
};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_postgres::error::SqlState;
use tokio_postgres::types::{FromSql, ToSql, Type};
use tokio_postgres::{Client, Row, SimpleQueryMessage, SimpleQueryRow, Statement};

use crate::connect::{OpenCursor, QueryClient};
use crate::map_error;
use crate::syntax::pg_syntax;
use crate::types::{Param, column_meta, parse_bytea, parse_cell};

type Params<'a> = [&'a (dyn ToSql + Sync)];

/// A table's name and live columns by attribute number, for column origins.
pub(crate) struct Relation {
    table: TableRef,
    columns: HashMap<i16, String>,
}

/// Relations by oid; `None` for an oid that names no table (dropped since).
pub(crate) type OriginCache = HashMap<u32, Option<Relation>>;

impl QueryClient {
    /// Ends `sqlr_cursor`. In its own transaction it commits that transaction, rolling back
    /// when it is aborted (after a cancel or a failed fetch); in the user's transaction block
    /// it only closes the cursor. Clears `cursor` either way.
    async fn finish_cursor(&mut self) -> Result<(), DriverError> {
        match self.cursor.take() {
            None => Ok(()),
            Some(OpenCursor::OwnTransaction) => {
                match self.client.simple_query("CLOSE sqlr_cursor; COMMIT").await {
                    Ok(_) => Ok(()),
                    Err(_) => self
                        .client
                        .simple_query("ROLLBACK")
                        .await
                        .map(drop)
                        .map_err(map_error),
                }
            }
            Some(OpenCursor::UserTransaction) => self
                .client
                .simple_query("CLOSE sqlr_cursor")
                .await
                .map(drop)
                .map_err(map_error),
        }
    }
}

/// Runs `sql` with `values` bound to its `$n` placeholders (none for a plain statement).
pub(crate) async fn execute(
    query: Arc<Mutex<QueryClient>>,
    sql: &str,
    values: &[Value],
) -> Result<Execution, DriverError> {
    let sql = sql.trim_end_matches(|c: char| c.is_whitespace() || c == ';');
    let bound: Vec<Param<'_>> = values.iter().map(Param).collect();
    let params: Vec<&(dyn ToSql + Sync)> = bound.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
    let mut guard = query.lock_owned().await;
    guard.finish_cursor().await?;

    let stmt = guard.client.prepare(sql).await.map_err(map_error)?;
    if stmt.columns().is_empty() {
        // DDL may rename or drop the tables origins point to (and a rollback may undo such
        // DDL); starting, committing or marking a transaction cannot.
        if !keeps_names(sql) {
            guard.origins.clear();
        }
        let n = guard
            .client
            .execute(&stmt, &params)
            .await
            .map_err(map_error)?;
        return Ok(Execution::Affected(n));
    }
    let columns = column_metas(&mut guard, &stmt).await?;
    drop(stmt);

    let mode = if is_cursor_query(sql) && declare_cursor(&mut guard, sql, &params).await? {
        Mode::Cursor { exhausted: false }
    } else if params.is_empty() {
        Mode::Text(Box::pin(
            guard
                .client
                .simple_query_raw(sql)
                .await
                .map_err(map_error)?,
        ))
    } else {
        let (stream, text) = stream_with_params(&guard.client, sql, &columns, &params).await?;
        Mode::Binary {
            stream: Box::pin(stream),
            text,
        }
    };
    Ok(Execution::Rows(Box::new(PgRowStream {
        guard: Some(guard),
        columns,
        mode,
    })))
}

/// Whether `sql` is a transaction verb that cannot change any table's name.
fn keeps_names(sql: &str) -> bool {
    use sqlr_core::{StatementClass, TxVerb};
    matches!(
        pg_syntax().classify(sql),
        StatementClass::Transaction(
            TxVerb::Begin | TxVerb::Commit | TxVerb::Savepoint(_) | TxVerb::Release(_)
        )
    )
}

const ORIGINS_SQL: &str = "\
SELECT c.oid, n.nspname::text, c.relname::text, a.attnum, a.attname::text
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
WHERE c.oid = ANY($1)";

/// The statement's columns with their origins. Relations are looked up once per oid on this
/// connection (in its transaction, so tables it created but did not commit resolve too).
async fn column_metas(
    guard: &mut QueryClient,
    stmt: &Statement,
) -> Result<Arc<[ColumnMeta]>, DriverError> {
    let mut missing: Vec<u32> = stmt
        .columns()
        .iter()
        .filter_map(|c| c.table_oid())
        .filter(|oid| *oid != 0 && !guard.origins.contains_key(oid))
        .collect();
    missing.sort_unstable();
    missing.dedup();
    if !missing.is_empty() {
        let rows = guard
            .client
            .query(ORIGINS_SQL, &[&missing])
            .await
            .map_err(map_error)?;
        for oid in missing {
            guard.origins.insert(oid, None);
        }
        for row in rows {
            let relation = guard
                .origins
                .entry(row.get(0))
                .or_default()
                .get_or_insert_with(|| Relation {
                    table: TableRef {
                        database: None,
                        schema: Some(row.get(1)),
                        name: row.get(2),
                    },
                    columns: HashMap::new(),
                });
            relation.columns.insert(row.get(3), row.get(4));
        }
    }
    Ok(stmt
        .columns()
        .iter()
        .map(|c| {
            let origin = c.table_oid().zip(c.column_id()).and_then(|(oid, attnum)| {
                let relation = guard.origins.get(&oid)?.as_ref()?;
                Some(ColumnOrigin {
                    table: relation.table.clone(),
                    column: relation.columns.get(&attnum)?.clone(),
                })
            });
            column_meta(c.type_(), c.name()).with_origin(origin)
        })
        .collect())
}

/// Confirms that every cached origin naming `table` still names the same relation: another
/// session may have renamed or dropped it since its oid was cached, and then the name now
/// stands for another table (or none). On a mismatch the cache is cleared, so the next
/// query reads fresh origins.
pub(crate) async fn check_origin(
    query: &Arc<Mutex<QueryClient>>,
    table: &TableRef,
) -> Result<(), DriverError> {
    let mut guard = query.lock().await;
    guard.finish_cursor().await?;
    let oids: Vec<u32> = guard
        .origins
        .iter()
        .filter_map(|(oid, relation)| {
            relation
                .as_ref()
                .filter(|r| r.table.schema == table.schema && r.table.name == table.name)
                .map(|_| *oid)
        })
        .collect();
    if oids.is_empty() {
        return Ok(());
    }
    let rows = guard
        .client
        .query(
            "SELECT c.oid, n.nspname::text, c.relname::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.oid = ANY($1)",
            &[&oids],
        )
        .await
        .map_err(map_error)?;
    let current: HashMap<u32, (String, String)> = rows
        .iter()
        .map(|r| (r.get(0), (r.get(1), r.get(2))))
        .collect();
    let unchanged = oids.iter().all(|oid| {
        current.get(oid).is_some_and(|(schema, name)| {
            table.schema.as_deref() == Some(schema.as_str()) && *name == table.name
        })
    });
    if unchanged {
        return Ok(());
    }
    guard.origins.clear();
    let name = match &table.schema {
        Some(schema) => format!("{schema}.{}", table.name),
        None => table.name.clone(),
    };
    Err(DriverError::new(
        ErrorKind::Other,
        format!(
            "Table {name} was renamed or dropped since this result was read; re-run the query \
             before editing"
        ),
    ))
}

/// `DECLARE` through the simple protocol, or through the extended one when it binds
/// parameters.
async fn run_declare(
    client: &Client,
    declare: &str,
    params: &Params<'_>,
) -> Result<(), tokio_postgres::Error> {
    if params.is_empty() {
        client.simple_query(declare).await.map(drop)
    } else {
        client.execute(declare, params).await.map(drop)
    }
}

/// Opens `sqlr_cursor`: inside the user's transaction block when one is open, otherwise in a
/// new transaction. `Ok(false)` means the statement cannot be a cursor and must be buffered
/// instead: 0A000 for a data-modifying CTE, 42601 for a top-level DML with a `WITH` clause
/// (`DECLARE` accepts only a SelectStmt; `prepare` already proved the statement itself parses).
async fn declare_cursor(
    guard: &mut QueryClient,
    sql: &str,
    params: &Params<'_>,
) -> Result<bool, DriverError> {
    let declare = format!("DECLARE sqlr_cursor NO SCROLL CURSOR FOR {sql}");
    let client = &guard.client;
    if in_transaction_block(client).await? {
        // The savepoint keeps a rejected DECLARE from aborting the user's block.
        client
            .simple_query("SAVEPOINT sqlr_declare")
            .await
            .map_err(map_error)?;
        let declared = run_declare(client, &declare, params).await;
        let undo = match &declared {
            Ok(_) => "RELEASE SAVEPOINT sqlr_declare",
            Err(_) => "ROLLBACK TO SAVEPOINT sqlr_declare; RELEASE SAVEPOINT sqlr_declare",
        };
        let undone = client.simple_query(undo).await;
        return match declared {
            Ok(_) => {
                undone.map_err(map_error)?;
                guard.cursor = Some(OpenCursor::UserTransaction);
                Ok(true)
            }
            Err(e) => buffer_instead(e, undone),
        };
    }

    client.simple_query("BEGIN").await.map_err(map_error)?;
    guard.cursor = Some(OpenCursor::OwnTransaction);
    let Err(e) = run_declare(&guard.client, &declare, params).await else {
        return Ok(true);
    };
    let rollback = guard.client.simple_query("ROLLBACK").await;
    guard.cursor = None;
    buffer_instead(e, rollback)
}

/// After a failed `DECLARE` and its `undo`: `Ok(false)` when the statement just cannot be a
/// cursor, the error otherwise.
fn buffer_instead(
    e: tokio_postgres::Error,
    undo: Result<Vec<SimpleQueryMessage>, tokio_postgres::Error>,
) -> Result<bool, DriverError> {
    let not_a_cursor = [SqlState::FEATURE_NOT_SUPPORTED, SqlState::SYNTAX_ERROR]
        .iter()
        .any(|code| e.code() == Some(code));
    if !not_a_cursor {
        return Err(map_error(e));
    }
    undo.map_err(map_error)?;
    Ok(false)
}

/// Whether the user has a transaction block open. Outside a block each statement is its own
/// transaction, so `now()` (the transaction's start) equals `statement_timestamp()`; inside
/// one, `now()` is when the block began, an earlier message.
async fn in_transaction_block(client: &Client) -> Result<bool, DriverError> {
    let messages = client
        .simple_query("SELECT now() = statement_timestamp()")
        .await
        .map_err(map_error)?;
    let same = data_rows(messages)
        .first()
        .and_then(|row| row.get(0).map(|v| v == "t"));
    same.map(|same| !same)
        .ok_or_else(|| protocol("transaction check returned no row"))
}

fn data_rows(messages: Vec<SimpleQueryMessage>) -> Vec<SimpleQueryRow> {
    messages
        .into_iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::Row(row) => Some(row),
            _ => None,
        })
        .collect()
}

fn protocol(message: impl std::fmt::Display) -> DriverError {
    DriverError::new(ErrorKind::Protocol, message.to_string())
}

/// Streams a row-returning statement with parameters that cannot be a cursor. Its rows
/// arrive in binary format. An `INSERT`/`UPDATE`/`DELETE … RETURNING` is wrapped so that every
/// column comes back through its type's text output (the same text a cursor fetch yields); any
/// other statement (a data-modifying `WITH`, `EXPLAIN`, …) is decoded from binary for the
/// common types.
async fn stream_with_params(
    client: &Client,
    sql: &str,
    columns: &[ColumnMeta],
    params: &Params<'_>,
) -> Result<(tokio_postgres::RowStream, bool), DriverError> {
    let keyword = first_keyword(sql);
    let wrappable = ["INSERT", "UPDATE", "DELETE"]
        .iter()
        .any(|k| keyword.eq_ignore_ascii_case(k));
    if !wrappable {
        return client
            .query_raw(sql, params.iter().copied())
            .await
            .map(|stream| (stream, false))
            .map_err(map_error);
    }
    let names: Vec<String> = (1..=columns.len()).map(|i| format!("c{i}")).collect();
    // `bool::text` is `true`/`false`, not the output function's `t`/`f`: read it binary.
    let projection: Vec<String> = columns
        .iter()
        .zip(&names)
        .map(|(meta, name)| match meta.kind {
            ColumnKind::Bool => name.clone(),
            _ => format!("{name}::text"),
        })
        .collect();
    let wrapped = format!(
        "WITH sqlr_w({}) AS (\n{sql}\n) SELECT {} FROM sqlr_w",
        names.join(", "),
        projection.join(", ")
    );
    client
        .query_raw(wrapped.as_str(), params.iter().copied())
        .await
        .map(|stream| (stream, true))
        .map_err(map_error)
}

fn get<'a, T: FromSql<'a>>(row: &'a Row, i: usize) -> Result<Option<T>, DriverError> {
    row.try_get(i).map_err(protocol)
}

/// A row whose columns are `text`, except booleans.
fn text_row(row: &Row, columns: &[ColumnMeta]) -> Result<Vec<Value>, DriverError> {
    let mut values = Vec::with_capacity(columns.len());
    for (i, meta) in columns.iter().enumerate() {
        values.push(match meta.kind {
            ColumnKind::Bool => get::<bool>(row, i)?.map_or(Value::Null, Value::Bool),
            ColumnKind::Bytes => match get::<&str>(row, i)? {
                None => Value::Null,
                Some(text) => {
                    let mut bytes = Vec::new();
                    parse_bytea(text, &mut bytes)?;
                    Value::Bytes(bytes)
                }
            },
            kind => Value::from_cell(parse_cell(kind, get::<&str>(row, i)?)?),
        });
    }
    Ok(values)
}

/// A binary-format row of common types.
fn binary_row(row: &Row) -> Result<Vec<Value>, DriverError> {
    let mut values = Vec::with_capacity(row.len());
    for (i, column) in row.columns().iter().enumerate() {
        let ty = column.type_();
        let value = match *ty {
            Type::BOOL => get::<bool>(row, i)?.map(Value::Bool),
            Type::INT2 => get::<i16>(row, i)?.map(|v| Value::Int(v.into())),
            Type::INT4 => get::<i32>(row, i)?.map(|v| Value::Int(v.into())),
            Type::INT8 => get::<i64>(row, i)?.map(Value::Int),
            Type::OID => get::<u32>(row, i)?.map(|v| Value::Int(v.into())),
            Type::FLOAT4 => get::<f32>(row, i)?.map(|v| Value::Float(v.into())),
            Type::FLOAT8 => get::<f64>(row, i)?.map(Value::Float),
            Type::BYTEA => get::<Vec<u8>>(row, i)?.map(Value::Bytes),
            _ if <&str as FromSql>::accepts(ty) => {
                get::<&str>(row, i)?.map(|v| Value::Text(v.to_string()))
            }
            _ => {
                return Err(DriverError::new(
                    ErrorKind::Unsupported,
                    format!(
                        "column `{}` of type {ty} cannot be returned by this statement when it \
                         has parameters",
                        column.name()
                    ),
                ));
            }
        };
        values.push(value.unwrap_or(Value::Null));
    }
    Ok(values)
}

fn build_chunk(
    columns: &Arc<[ColumnMeta]>,
    rows: &[SimpleQueryRow],
) -> Result<ResultChunk, DriverError> {
    let mut builder = ChunkBuilder::new(columns.clone());
    // `bytea` text decodes into these buffers, one per column, before the row borrows them.
    let mut bytes: Vec<Vec<u8>> = vec![Vec::new(); columns.len()];
    for row in rows {
        if row.len() != columns.len() {
            return Err(protocol(format!(
                "row has {} values, expected {}",
                row.len(),
                columns.len()
            )));
        }
        for (i, meta) in columns.iter().enumerate() {
            if meta.kind == ColumnKind::Bytes
                && let Some(text) = row.try_get(i).map_err(protocol)?
            {
                parse_bytea(text, &mut bytes[i])?;
            }
        }
        let mut cells = Vec::with_capacity(columns.len());
        for (i, meta) in columns.iter().enumerate() {
            let text = row.try_get(i).map_err(protocol)?;
            cells.push(match (meta.kind, text) {
                (ColumnKind::Bytes, Some(_)) => Cell::Bytes(&bytes[i]),
                (kind, text) => parse_cell(kind, text)?,
            });
        }
        builder.push_row(&cells).map_err(protocol)?;
    }
    Ok(builder.finish())
}

/// The first word of `sql` after comments, lexed with the PostgreSQL syntax.
fn first_keyword(sql: &str) -> &str {
    let mut tokens = Vec::new();
    pg_syntax().lex(sql, LexState::Normal, &mut tokens);
    tokens
        .iter()
        .find(|t| t.kind != TokenKind::Comment)
        .filter(|t| {
            matches!(
                t.kind,
                TokenKind::Keyword | TokenKind::Function | TokenKind::Ident
            )
        })
        .map_or("", |t| &sql[t.start..t.end])
}

fn is_cursor_query(sql: &str) -> bool {
    let keyword = first_keyword(sql);
    ["SELECT", "WITH", "VALUES", "TABLE"]
        .iter()
        .any(|k| keyword.eq_ignore_ascii_case(k))
}

enum Mode {
    Cursor {
        exhausted: bool,
    },
    Text(Pin<Box<tokio_postgres::SimpleQueryStream>>),
    Binary {
        stream: Pin<Box<tokio_postgres::RowStream>>,
        text: bool,
    },
    Closed,
}

/// Holds the query client until it is closed or dropped; the next `execute` waits for that.
struct PgRowStream {
    /// `None` once closed.
    guard: Option<OwnedMutexGuard<QueryClient>>,
    columns: Arc<[ColumnMeta]>,
    mode: Mode,
}

#[async_trait::async_trait]
impl RowStream for PgRowStream {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }

    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        // `FETCH 0` would re-read the current row.
        let max_rows = max_rows.max(1);
        let Self {
            guard,
            columns,
            mode,
        } = self;
        match mode {
            Mode::Closed => Ok(None),
            Mode::Text(stream) => {
                let mut rows = Vec::new();
                let mut exhausted = false;
                while rows.len() < max_rows {
                    match stream.try_next().await.map_err(map_error)? {
                        Some(SimpleQueryMessage::Row(row)) => rows.push(row),
                        Some(_) => {}
                        None => {
                            exhausted = true;
                            break;
                        }
                    }
                }
                if exhausted {
                    *mode = Mode::Closed;
                }
                if rows.is_empty() {
                    Ok(None)
                } else {
                    build_chunk(columns, &rows).map(Some)
                }
            }
            Mode::Binary { stream, text } => {
                let mut builder = ChunkBuilder::new(columns.clone());
                let mut exhausted = false;
                while builder.len() < max_rows {
                    let Some(row) = stream.try_next().await.map_err(map_error)? else {
                        exhausted = true;
                        break;
                    };
                    let values = if *text {
                        text_row(&row, columns)?
                    } else {
                        binary_row(&row)?
                    };
                    let cells: Vec<_> = values.iter().map(Value::as_cell).collect();
                    builder.push_row(&cells).map_err(protocol)?;
                }
                if exhausted {
                    *mode = Mode::Closed;
                }
                if builder.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(builder.finish()))
                }
            }
            Mode::Cursor { exhausted } => {
                let (false, Some(guard)) = (*exhausted, guard.as_mut()) else {
                    return Ok(None);
                };
                let fetch = format!("FETCH {max_rows} FROM sqlr_cursor");
                let rows = match guard.client.simple_query(&fetch).await {
                    Ok(messages) => data_rows(messages),
                    Err(e) => {
                        // The transaction is aborted. sqlr's own is rolled back by `close` or
                        // the next `execute`; the user's block is left for the user to end, and
                        // ending it drops the cursor.
                        *exhausted = true;
                        if guard.cursor == Some(OpenCursor::UserTransaction) {
                            guard.cursor = None;
                        }
                        return Err(map_error(e));
                    }
                };
                if rows.len() < max_rows {
                    *exhausted = true;
                    guard.finish_cursor().await?;
                }
                if rows.is_empty() {
                    return Ok(None);
                }
                build_chunk(columns, &rows).map(Some)
            }
        }
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        // Dropping a wire stream lets tokio-postgres discard remaining responses with
        // bounded memory. Do not cancel row-returning writes: their side effects must
        // complete even when the viewer stops fetching RETURNING rows.
        self.mode = Mode::Closed;
        // Releasing the guard lets the next `execute` run even if this stream is kept alive.
        if let Some(mut guard) = self.guard.take() {
            guard.finish_cursor().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_keyword_skips_whitespace_and_comments() {
        assert_eq!(first_keyword("  select 1"), "select");
        assert_eq!(
            first_keyword("-- note\n\tWITH x AS (SELECT 1) TABLE x"),
            "WITH"
        );
        assert_eq!(first_keyword("/* a /* nested */ b */ VALUES (1)"), "VALUES");
        assert_eq!(first_keyword("-- only a comment"), "");
        assert_eq!(first_keyword("/* unterminated SELECT"), "");
        assert_eq!(first_keyword("(SELECT 1)"), "");
    }

    #[test]
    fn cursor_queries_by_first_keyword() {
        for sql in [
            "SELECT 1",
            "with x as (select 1) select * from x",
            "Values (1)",
            "TABLE t",
        ] {
            assert!(is_cursor_query(sql), "{sql}");
        }
        for sql in [
            "SHOW server_version",
            "EXPLAIN SELECT 1",
            "INSERT INTO t VALUES (1) RETURNING x",
            "SELECTx",
        ] {
            assert!(!is_cursor_query(sql), "{sql}");
        }
    }
}
