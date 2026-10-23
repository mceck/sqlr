//! What a transfer into a table needs besides inserting: the `CREATE TABLE` of a new
//! table, and the file that collects rejected rows.

use std::path::PathBuf;

use sqlr_core::{
    Dialect, DriverError, ErrorKind, LexState, ObjectDef, TableRef, Value, new_table_def,
};
use tokio::io::{AsyncWriteExt, BufWriter};

/// The statements that create `table` from source columns `(name, type, forced type)` of
/// dialect `from`, their types mapped to `dialect`'s `to` id (a forced type is used as it
/// is), and the mapping warnings.
pub(super) fn create_table_statements(
    dialect: &dyn Dialect,
    from: &str,
    to: &str,
    table: &TableRef,
    columns: &[(String, String, Option<String>)],
) -> Result<(Vec<String>, Vec<String>), DriverError> {
    let generator = dialect.ddl_gen().ok_or_else(|| {
        DriverError::new(
            ErrorKind::Unsupported,
            "this connection cannot create tables",
        )
    })?;
    let typed: Vec<(String, String)> = columns
        .iter()
        .map(|(name, ty, _)| (name.clone(), ty.clone()))
        .collect();
    let (mut def, warnings) = new_table_def(table, from, to, &typed);
    for (column, (_, _, forced)) in def.columns.iter_mut().zip(columns) {
        if let Some(forced) = forced {
            column.type_name = forced.clone();
        }
    }
    let script = generator.create_ddl(&ObjectDef::Table(def))?;
    Ok((split_statements(dialect, &script), warnings))
}

/// The statements of a script, without their terminating `;`.
pub(super) fn split_statements(dialect: &dyn Dialect, script: &str) -> Vec<String> {
    dialect
        .syntax()
        .split(script, LexState::default())
        .into_iter()
        .map(|range| script[range].trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// A CSV of the rows a transfer rejected: their line, the reason and their values. The file
/// is created when the first row is rejected, so a clean run leaves none.
pub(super) struct RejectedFile {
    path: PathBuf,
    names: Option<Vec<String>>,
    out: Option<BufWriter<tokio::fs::File>>,
}

impl RejectedFile {
    pub fn new(path: PathBuf, names: Option<Vec<String>>) -> Self {
        Self {
            path,
            names,
            out: None,
        }
    }

    pub async fn add(&mut self, line: u64, reason: &str, row: &[Value]) -> Result<(), DriverError> {
        let io = |e: std::io::Error| {
            DriverError::new(ErrorKind::Other, format!("{}: {e}", self.path.display()))
        };
        if self.out.is_none() {
            let file = tokio::fs::File::create(&self.path).await.map_err(io)?;
            let mut out = BufWriter::new(file);
            let mut header = String::from("line,reason");
            match &self.names {
                Some(names) => {
                    for name in names {
                        header.push(',');
                        header.push_str(&csv_field(name));
                    }
                }
                None => {
                    for i in 1..=row.len() {
                        header.push_str(&format!(",column {i}"));
                    }
                }
            }
            header.push('\n');
            out.write_all(header.as_bytes()).await.map_err(io)?;
            self.out = Some(out);
        }
        let mut text = format!("{line},{}", csv_field(reason));
        for value in row {
            text.push(',');
            if !value.is_null() {
                text.push_str(&csv_field(&value.display()));
            }
        }
        text.push('\n');
        let out = self.out.as_mut().expect("opened above");
        out.write_all(text.as_bytes()).await.map_err(io)
    }

    /// Flushes the file; `Some(path)` when rows were rejected.
    pub async fn finish(mut self) -> Result<Option<PathBuf>, DriverError> {
        let Some(mut out) = self.out.take() else {
            return Ok(None);
        };
        out.flush().await.map_err(|e| {
            DriverError::new(ErrorKind::Other, format!("{}: {e}", self.path.display()))
        })?;
        Ok(Some(self.path))
    }
}

fn csv_field(text: &str) -> String {
    // This file is opened in a spreadsheet too: keep a value from becoming a formula.
    let text = if !text.starts_with('\'')
        && text
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '=' | '+' | '-' | '@' | '\t' | '\r' | '\n'))
    {
        format!("'{text}")
    } else {
        text.to_string()
    };
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}
