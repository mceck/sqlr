//! Running statements and streaming their rows.

use std::sync::Arc;

use async_trait::async_trait;
use hdbconnect_async::{Connection as HdbConnection, HdbReturnValue, ResultSet};
use sqlr_driver_sdk::{
    Cell, ChunkBuilder, ColumnMeta, ColumnOrigin, DriverError, Execution, ResultChunk, RowStream,
    TableRef,
};

use crate::{errors, types};

pub(crate) async fn execute(conn: &HdbConnection, sql: &str) -> Result<Execution, DriverError> {
    let response = conn.statement(sql).await.map_err(errors::map)?;
    match response.into_single_retval().map_err(errors::map)? {
        HdbReturnValue::ResultSet(rs) => Ok(Execution::Rows(Box::new(HanaRows::new(rs)))),
        HdbReturnValue::AffectedRows(counts) => {
            Ok(Execution::Affected(counts.iter().map(|n| *n as u64).sum()))
        }
        _ => Ok(Execution::Affected(0)),
    }
}

pub(crate) struct HanaRows {
    rs: ResultSet,
    columns: Arc<[ColumnMeta]>,
    done: bool,
}

impl HanaRows {
    pub(crate) fn new(rs: ResultSet) -> Self {
        let columns: Vec<ColumnMeta> = rs
            .metadata()
            .iter()
            .map(|field| {
                let shape = types::column_kind(field.type_id());
                let origin = (!field.tablename().is_empty()).then(|| ColumnOrigin {
                    table: TableRef {
                        database: None,
                        schema: Some(field.schemaname().to_string()).filter(|s| !s.is_empty()),
                        name: field.tablename().to_string(),
                    },
                    column: field.columnname().to_string(),
                });
                ColumnMeta::new(
                    field.displayname().to_string(),
                    types::type_name(field.type_id()),
                    shape.kind,
                    shape.hint,
                )
                .with_origin(origin)
            })
            .collect();
        Self {
            rs,
            columns: Arc::from(columns),
            done: false,
        }
    }
}

#[async_trait]
impl RowStream for HanaRows {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }

    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        if self.done {
            return Ok(None);
        }
        let mut builder = ChunkBuilder::new(self.columns.clone());
        while builder.len() < max_rows.max(1) {
            let Some(mut row) = self.rs.next_row().await.map_err(errors::map)? else {
                self.done = true;
                break;
            };
            let mut values = Vec::with_capacity(self.columns.len());
            while let Some(v) = row.next_value() {
                values.push(types::value(v).await?);
            }
            let cells: Vec<Cell<'_>> = values.iter().map(|v| v.as_cell()).collect();
            builder.push_row(&cells).map_err(|e| {
                DriverError::new(sqlr_driver_sdk::ErrorKind::Protocol, e.to_string())
            })?;
        }
        Ok((!builder.is_empty()).then(|| builder.finish()))
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        self.done = true;
        Ok(())
    }
}
