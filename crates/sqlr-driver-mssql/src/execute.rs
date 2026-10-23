//! Query execution: results buffered from TDS, row counts and paging.

use std::sync::Arc;

use futures_util::TryStreamExt;
use sqlr_core::{
    Cell, ChunkBuilder, ColumnMeta, DriverError, ErrorKind, Execution, LexState, ResultChunk,
    RowStream, TokenKind, Value, trim_statement_in,
};
use tiberius::{ColumnData, QueryItem, ToSql};

use crate::browse::MsClient;
use crate::map_error;
use crate::syntax::tsql_syntax;
use crate::types;

pub(crate) async fn execute(
    client: &mut MsClient,
    sql: &str,
    params: &[Value],
) -> Result<Execution, DriverError> {
    let syntax = tsql_syntax();
    let sql = trim_statement_in(&syntax, sql);
    if !returns_rows(&sql) {
        let encoded: Vec<ColumnData<'static>> = params.iter().map(types::encode).collect();
        let refs: Vec<&dyn ToSql> = encoded.iter().map(|v| v as &dyn ToSql).collect();
        let result = client
            .execute(sql.as_ref(), &refs)
            .await
            .map_err(map_error)?;
        return Ok(Execution::Affected(result.total()));
    }

    let encoded: Vec<ColumnData<'static>> = params.iter().map(types::encode).collect();
    let refs: Vec<&dyn ToSql> = encoded.iter().map(|v| v as &dyn ToSql).collect();
    let stream = client.query(sql.as_ref(), &refs).await.map_err(map_error)?;
    let items: Vec<QueryItem> = stream.try_collect().await.map_err(map_error)?;

    let metadata = items.iter().find_map(|item| match item {
        QueryItem::Metadata(meta) if meta.result_index() == 0 => Some(meta),
        _ => None,
    });
    let Some(metadata) = metadata else {
        return Ok(Execution::Affected(0));
    };
    let columns: Arc<[ColumnMeta]> = metadata
        .columns()
        .iter()
        .map(types::column_meta)
        .collect::<Vec<_>>()
        .into();
    let rows: Vec<Vec<Value>> = items
        .iter()
        .filter_map(QueryItem::as_row)
        .filter(|row| row.result_index() == 0)
        .map(|row| row.cells().map(|(_, data)| types::decode(data)).collect())
        .collect();
    Ok(Execution::Rows(Box::new(MsRows {
        columns,
        rows,
        at: 0,
        done: false,
    })))
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
        "SELECT" | "WITH" | "EXEC" | "EXECUTE" | "DECLARE" | "PRINT" | "DBCC" | "VALUES"
    ) {
        return true;
    }
    // `UPDATE … OUTPUT`, `INSERT … OUTPUT` and `DELETE … OUTPUT` return rows.
    sql.to_ascii_uppercase().contains(" OUTPUT ")
}

/// A result set already read from the wire, paged from memory.
struct MsRows {
    columns: Arc<[ColumnMeta]>,
    rows: Vec<Vec<Value>>,
    at: usize,
    done: bool,
}

#[async_trait::async_trait]
impl RowStream for MsRows {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }

    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        if self.done || self.at >= self.rows.len() {
            self.done = true;
            return Ok(None);
        }
        let end = (self.at + max_rows.max(1)).min(self.rows.len());
        let mut builder = ChunkBuilder::new(self.columns.clone());
        for row in &self.rows[self.at..end] {
            let cells: Vec<Cell<'_>> = row.iter().map(Value::as_cell).collect();
            builder.push_row(&cells).map_err(|e| {
                DriverError::new(ErrorKind::Other, format!("cannot build result chunk: {e}"))
            })?;
        }
        self.at = end;
        if self.at >= self.rows.len() {
            self.done = true;
        }
        Ok(Some(builder.finish()))
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        self.rows.clear();
        self.at = 0;
        self.done = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::returns_rows;

    #[test]
    fn only_rowsets_are_queried() {
        assert!(returns_rows("SELECT 1"));
        assert!(returns_rows(
            "  -- note\n WITH x AS (SELECT 1) SELECT * FROM x"
        ));
        assert!(returns_rows("INSERT INTO t OUTPUT inserted.id VALUES (1)"));
        assert!(!returns_rows("INSERT INTO t VALUES (1)"));
        assert!(!returns_rows("UPDATE t SET a = 1"));
    }
}
