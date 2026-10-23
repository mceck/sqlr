//! Query execution: prefetched Oracle rows, cursor paging and row counts.

use std::sync::Arc;

use oracle_rs::{ColumnInfo, Connection, Value as OracleValue};
use sqlr_core::{
    Cell, ChunkBuilder, ColumnMeta, DriverError, ErrorKind, Execution, ResultChunk, RowStream,
    Value, trim_statement_in,
};

use crate::map_error;
use crate::syntax::oracle_syntax;
use crate::types;

pub(crate) async fn execute(
    conn: Arc<Connection>,
    sql: &str,
    params: &[Value],
) -> Result<Execution, DriverError> {
    let sql = trim_statement_in(&oracle_syntax(), sql);
    let binds: Vec<OracleValue> = params.iter().map(types::encode).collect();
    let result = conn
        .execute(sql.as_ref(), &binds)
        .await
        .map_err(map_error)?;
    if result.columns.is_empty() {
        return Ok(Execution::Affected(result.rows_affected));
    }
    let metas: Arc<[ColumnMeta]> = result
        .columns
        .iter()
        .map(types::column_meta)
        .collect::<Vec<_>>()
        .into();
    Ok(Execution::Rows(Box::new(OracleRows {
        conn,
        columns: result.columns,
        metas,
        rows: result.rows,
        at: 0,
        has_more: result.has_more_rows,
        cursor_id: result.cursor_id,
        done: false,
    })))
}

struct OracleRows {
    conn: Arc<Connection>,
    columns: Vec<ColumnInfo>,
    metas: Arc<[ColumnMeta]>,
    rows: Vec<oracle_rs::Row>,
    at: usize,
    has_more: bool,
    cursor_id: u16,
    done: bool,
}

#[async_trait::async_trait]
impl RowStream for OracleRows {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.metas.clone()
    }

    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        let max_rows = max_rows.max(1);
        if self.at >= self.rows.len() {
            if self.done || !self.has_more {
                self.done = true;
                return Ok(None);
            }
            let fetch = u32::try_from(max_rows).unwrap_or(u32::MAX);
            let result = self
                .conn
                .fetch_more(self.cursor_id, &self.columns, fetch)
                .await
                .map_err(map_error)?;
            self.rows = result.rows;
            self.at = 0;
            self.has_more = result.has_more_rows;
            if result.cursor_id != 0 {
                self.cursor_id = result.cursor_id;
            }
            // A server that claims more rows but sends none would loop forever.
            if self.rows.is_empty() {
                self.done = true;
                return Ok(None);
            }
        }
        let end = (self.at + max_rows).min(self.rows.len());
        let mut builder = ChunkBuilder::new(self.metas.clone());
        for row in &self.rows[self.at..end] {
            let values: Vec<Value> = row
                .values()
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let ty = self
                        .columns
                        .get(index)
                        .map_or(oracle_rs::OracleType::Varchar, |c| c.oracle_type);
                    types::decode(value, ty)
                })
                .collect();
            let cells: Vec<Cell<'_>> = values.iter().map(Value::as_cell).collect();
            builder.push_row(&cells).map_err(|e| {
                DriverError::new(ErrorKind::Other, format!("cannot build result chunk: {e}"))
            })?;
        }
        self.at = end;
        if self.at >= self.rows.len() && !self.has_more {
            self.done = true;
        }
        Ok(Some(builder.finish()))
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        self.rows.clear();
        self.at = 0;
        self.has_more = false;
        self.done = true;
        Ok(())
    }
}
