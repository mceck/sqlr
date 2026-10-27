//! ES|QL (`_query`) and SQL (`_sql`) results as row streams.
//!
//! Columns are `Mixed`: a field can hold several values (an array) in any row, so each
//! cell keeps its own type; the display hint follows the declared type.

use std::collections::VecDeque;
use std::sync::Arc;

use serde_json::{Value as Json, json};
use sqlr_core::{
    Cell, ChunkBuilder, ColumnKind, ColumnMeta, DisplayHint, DriverError, ErrorKind, ResultChunk,
    RowStream, Value,
};

use crate::client::{Body, EsClient};

fn hint_for(ty: &str) -> DisplayHint {
    match ty {
        "long" | "integer" | "short" | "byte" | "unsigned_long" | "double" | "float"
        | "half_float" | "scaled_float" | "counter_long" | "counter_integer" | "counter_double" => {
            DisplayHint::Number
        }
        "date" | "datetime" | "date_nanos" | "date_period" | "time_duration" => {
            DisplayHint::Temporal
        }
        "object" | "nested" | "geo_point" | "geo_shape" | "cartesian_point" | "cartesian_shape" => {
            DisplayHint::Json
        }
        _ => DisplayHint::Plain,
    }
}

/// Column metadata from `[{"name": …, "type": …}]`.
pub(crate) fn columns(cols: &Json) -> Vec<ColumnMeta> {
    cols.as_array()
        .map(|cols| {
            cols.iter()
                .map(|c| {
                    let ty = c["type"].as_str().unwrap_or("unknown");
                    ColumnMeta::new(
                        c["name"].as_str().unwrap_or("?"),
                        ty,
                        ColumnKind::Mixed,
                        hint_for(ty),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A JSON cell as an owned grid value; arrays and objects as compact JSON text.
pub(crate) fn cell(v: &Json) -> Value {
    match v {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => match n.as_u64() {
                Some(u) => Value::Text(u.to_string()),
                None => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
            },
        },
        Json::String(s) => Value::Text(s.clone()),
        other => Value::Text(other.to_string()),
    }
}

fn rows_of(values: &Json) -> VecDeque<Vec<Value>> {
    values
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    r.as_array()
                        .map(|r| r.iter().map(cell).collect())
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Rows held in memory, plus an SQL cursor for more.
pub(crate) struct EsRows {
    client: Arc<EsClient>,
    columns: Arc<[ColumnMeta]>,
    buffer: VecDeque<Vec<Value>>,
    cursor: Option<String>,
}

impl EsRows {
    /// An ES|QL response: `{columns, values}`.
    pub(crate) fn esql(client: Arc<EsClient>, body: &Json) -> Self {
        Self {
            client,
            columns: columns(&body["columns"]).into(),
            buffer: rows_of(&body["values"]),
            cursor: None,
        }
    }

    /// An SQL response: `{columns, rows, cursor?}`.
    pub(crate) fn sql(client: Arc<EsClient>, body: &Json) -> Self {
        Self {
            client,
            columns: columns(&body["columns"]).into(),
            buffer: rows_of(&body["rows"]),
            cursor: body["cursor"].as_str().map(String::from),
        }
    }
}

#[async_trait::async_trait]
impl RowStream for EsRows {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }

    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        let max = max_rows.max(1);
        while self.buffer.len() < max
            && let Some(cursor) = self.cursor.take()
        {
            let body = self
                .client
                .ok(
                    "POST",
                    "/_sql?format=json",
                    Body::Json(json!({ "cursor": cursor })),
                )
                .await?;
            self.buffer.extend(rows_of(&body["rows"]));
            self.cursor = body["cursor"].as_str().map(String::from);
        }
        if self.buffer.is_empty() {
            return Ok(None);
        }
        let mut b = ChunkBuilder::new(self.columns.clone());
        for _ in 0..max.min(self.buffer.len()) {
            let mut row = self
                .buffer
                .pop_front()
                .expect("the loop count is bounded by the buffer length");
            row.resize(self.columns.len(), Value::Null);
            let cells: Vec<Cell<'_>> = row.iter().map(Value::as_cell).collect();
            b.push_row(&cells)
                .map_err(|e| DriverError::new(ErrorKind::Protocol, format!("{e:?}")))?;
        }
        Ok(Some(b.finish()))
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        self.buffer.clear();
        if let Some(cursor) = self.cursor.take() {
            // Frees the server-side cursor; a failure here loses nothing.
            let _ = self
                .client
                .ok(
                    "POST",
                    "/_sql/close",
                    Body::Json(json!({ "cursor": cursor })),
                )
                .await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_and_columns() {
        let cols = columns(&json!([{"name": "n", "type": "long"}, {"name": "t", "type": "date"}]));
        assert_eq!(cols[0].hint, DisplayHint::Number);
        assert_eq!(cols[1].hint, DisplayHint::Temporal);
        assert_eq!(cell(&json!(3)), Value::Int(3));
        assert_eq!(cell(&json!(1.5)), Value::Float(1.5));
        assert_eq!(cell(&json!(u64::MAX)), Value::Text(u64::MAX.to_string()));
        assert_eq!(cell(&json!([1, 2])), Value::Text("[1,2]".into()));
    }
}
