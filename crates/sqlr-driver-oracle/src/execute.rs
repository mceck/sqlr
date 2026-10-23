//! Query execution: prefetched Oracle rows, cursor paging and row counts.

use std::sync::Arc;

use oracle_rs::{ColumnInfo, Connection, LobData, LobValue, Value as OracleValue};
use sqlr_core::{
    Cell, ChunkBuilder, ColumnMeta, DriverError, ErrorKind, Execution, ResultChunk, RowStream,
    Value, trim_statement_in,
};

use crate::map_error;
use crate::syntax::oracle_syntax;
use crate::types;

/// PL/SQL blocks and stored-program definitions end in `END;`: that semicolon belongs to
/// the program, so only plain SQL loses its terminator.
fn statement_text(sql: &str) -> std::borrow::Cow<'_, str> {
    if is_plsql(sql) {
        std::borrow::Cow::Borrowed(sql.trim())
    } else {
        trim_statement_in(&oracle_syntax(), sql)
    }
}

fn is_plsql(sql: &str) -> bool {
    let mut rest = sql.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, r)| r).trim_start();
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, r)| r).trim_start();
        } else {
            break;
        }
    }
    let mut words = rest
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_uppercase);
    match words.next().as_deref() {
        Some("BEGIN" | "DECLARE") => true,
        Some("CREATE") => {
            let mut next = words.next();
            if next.as_deref() == Some("OR") {
                words.next();
                next = words.next();
            }
            if matches!(next.as_deref(), Some("EDITIONABLE" | "NONEDITIONABLE")) {
                next = words.next();
            }
            matches!(
                next.as_deref(),
                Some("FUNCTION" | "PROCEDURE" | "PACKAGE" | "TRIGGER" | "TYPE")
            )
        }
        _ => false,
    }
}

pub(crate) async fn execute(
    conn: Arc<Connection>,
    sql: &str,
    params: &[Value],
) -> Result<Execution, DriverError> {
    let sql = statement_text(sql);
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
            let mut values: Vec<Value> = Vec::with_capacity(row.values().len());
            for (index, value) in row.values().iter().enumerate() {
                let ty = self
                    .columns
                    .get(index)
                    .map_or(oracle_rs::OracleType::Varchar, |c| c.oracle_type);
                values.push(match value {
                    // Larger LOBs arrive as locators that need a round trip to read.
                    OracleValue::Lob(LobValue::Locator(locator)) => {
                        match self.conn.read_lob(locator).await {
                            Ok(LobData::String(text)) => Value::Text(text),
                            Ok(LobData::Bytes(bytes)) => Value::Bytes(bytes.to_vec()),
                            Err(_) => types::decode(value, ty),
                        }
                    }
                    _ => types::decode(value, ty),
                });
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plsql_keeps_its_semicolon_and_sql_loses_it() {
        for sql in [
            "BEGIN NULL; END;",
            "  declare x number; begin null; end;",
            "-- note\nCREATE OR REPLACE FUNCTION f RETURN NUMBER IS BEGIN RETURN 1; END;",
            "/* c */ create or replace editionable procedure p is begin null; end;",
            "CREATE TRIGGER t BEFORE INSERT ON x BEGIN NULL; END;",
            "CREATE PACKAGE BODY p AS END;",
        ] {
            assert!(statement_text(sql).ends_with(';'), "{sql}");
        }
        for sql in [
            "SELECT 1 FROM dual;",
            "CREATE TABLE t (a NUMBER);",
            "CREATE OR REPLACE VIEW v AS SELECT 1 a FROM dual;",
            "",
        ] {
            assert!(!statement_text(sql).ends_with(';'), "{sql}");
        }
    }
}
