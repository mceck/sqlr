//! Excel workbooks (`.xlsx`): a sheet read into rows, and rows written to a sheet.
//!
//! Excel caps a sheet at 1 048 576 rows, so a sheet is read whole; the writer streams
//! rows to disk (constant-memory mode) and never holds more than the row it writes.

use std::path::Path;

use calamine::{Data, Reader, Xlsx, open_workbook};
use sqlr_core::Value;

use super::convert::civil_from_days;
use super::error::TransferFileError;

/// The columns and rows of `sheet` (the first sheet when `None`). With `header` the first
/// row names the columns, otherwise they are `column1`, `column2`…. Dates become ISO text,
/// formulas give their cached result, blank cells are NULL.
pub(super) fn read_sheet(
    path: &Path,
    sheet: Option<&str>,
    header: bool,
) -> Result<(Vec<String>, Vec<Vec<Value>>), TransferFileError> {
    let mut book: Xlsx<_> = open_workbook(path).map_err(|source| TransferFileError::Workbook {
        path: path.to_path_buf(),
        source,
    })?;
    let name = match sheet {
        Some(name) => name.to_string(),
        None => book
            .sheet_names()
            .first()
            .cloned()
            .ok_or(TransferFileError::NoSheets)?,
    };
    let range = book
        .worksheet_range(&name)
        .map_err(|source| TransferFileError::Sheet {
            name: name.clone(),
            source,
        })?;
    let mut rows = range.rows();
    let width = range.width();
    let names: Vec<String> = if header {
        match rows.next() {
            Some(first) => (0..width)
                .map(|i| match first.get(i) {
                    Some(Data::Empty) | None => format!("column{}", i + 1),
                    Some(cell) => cell_text(cell),
                })
                .collect(),
            None => Vec::new(),
        }
    } else {
        (1..=width).map(|i| format!("column{i}")).collect()
    };
    let rows = rows
        .map(|row| {
            (0..width)
                .map(|i| row.get(i).map_or(Value::Null, cell_value))
                .collect()
        })
        .collect();
    Ok((names, rows))
}

/// The names of a workbook's sheets.
pub(super) fn sheet_names(path: &Path) -> Result<Vec<String>, TransferFileError> {
    let book: Xlsx<_> = open_workbook(path).map_err(|source| TransferFileError::Workbook {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(book.sheet_names())
}

fn cell_value(cell: &Data) -> Value {
    match cell {
        Data::Empty => Value::Null,
        Data::Int(i) => Value::Int(*i),
        // Excel stores every number as a float: whole ones are the integers they look like.
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => Value::Int(*f as i64),
        Data::Float(f) => Value::Float(*f),
        Data::Bool(b) => Value::Bool(*b),
        Data::String(s) => Value::Text(s.clone()),
        Data::DateTime(d) => {
            let serial = d.as_f64();
            Value::Text(if d.is_duration() {
                duration_text(serial)
            } else {
                serial_to_iso(serial)
            })
        }
        Data::DateTimeIso(s) | Data::DurationIso(s) => Value::Text(s.clone()),
        Data::Error(e) => Value::Text(e.to_string()),
    }
}

fn cell_text(cell: &Data) -> String {
    match cell_value(cell) {
        Value::Text(s) => s,
        other => other.display().into_owned(),
    }
}

/// An Excel serial date (1900 system) as `YYYY-MM-DD`, with the time of day when it has one.
fn serial_to_iso(serial: f64) -> String {
    // 25569 days separate Excel's day 0 (1899-12-30) from the Unix epoch.
    let days = serial.floor();
    let mut seconds = ((serial - days) * 86_400.0).round() as i64;
    let mut days = days as i64 - 25_569;
    // Excel's 1900 leap-year bug: serials 1..59 are one day later than the 1899-12-30
    // epoch suggests (serial 60 is its fictitious 1900-02-29, which no calendar has).
    if serial < 61.0 {
        days += 1;
    }
    if seconds >= 86_400 {
        seconds -= 86_400;
        days += 1;
    }
    let (y, m, d) = civil_from_days(days);
    if seconds == 0 {
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    }
}

fn duration_text(days: f64) -> String {
    let seconds = (days * 86_400.0).round() as i64;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// Writes rows to a one-sheet workbook as they arrive.
pub(super) struct SheetWriter {
    book: rust_xlsxwriter::Workbook,
    row: u32,
    columns: usize,
}

/// Excel's limits: rows per sheet and characters per cell.
const MAX_ROWS: u32 = 1_048_576;
const MAX_CELL_CHARS: usize = 32_767;

impl SheetWriter {
    pub fn new(names: &[String]) -> Result<Self, TransferFileError> {
        let mut book = rust_xlsxwriter::Workbook::new();
        let sheet = book.add_worksheet_with_constant_memory()?;
        let bold = rust_xlsxwriter::Format::new().set_bold();
        for (i, name) in names.iter().enumerate() {
            sheet.write_string_with_format(0, i as u16, name, &bold)?;
        }
        Ok(Self {
            book,
            row: 1,
            columns: names.len(),
        })
    }

    pub fn write_row(&mut self, row: &[Value]) -> Result<(), TransferFileError> {
        if self.row >= MAX_ROWS {
            return Err(TransferFileError::SheetFull);
        }
        let sheet = self.book.worksheet_from_index(0)?;
        for (i, value) in row.iter().enumerate().take(self.columns) {
            let col = i as u16;
            let done = match value {
                Value::Null => continue,
                Value::Bool(b) => sheet.write_boolean(self.row, col, *b),
                // Excel stores numbers as doubles: above 2^53 a number would be silently
                // rounded, so the exact integer is written as text instead.
                Value::Int(n) if n.unsigned_abs() <= (1u64 << 53) => {
                    sheet.write_number(self.row, col, *n as f64)
                }
                Value::Int(n) => sheet.write_string(self.row, col, n.to_string()),
                Value::Float(f) => sheet.write_number(self.row, col, *f),
                Value::Text(s) => {
                    let chars = s.chars().count();
                    if chars > MAX_CELL_CHARS {
                        return Err(TransferFileError::CellTooLong {
                            row: self.row + 1,
                            column: col + 1,
                            chars,
                        });
                    }
                    sheet.write_string(self.row, col, s)
                }
                Value::Bytes(_) => {
                    let hex = value.display().into_owned();
                    sheet.write_string(self.row, col, &hex)
                }
            };
            done?;
        }
        self.row += 1;
        Ok(())
    }

    pub fn save(mut self, path: &Path) -> Result<(), TransferFileError> {
        Ok(self.book.save(path)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_dates_become_iso_text() {
        assert_eq!(serial_to_iso(25_569.0), "1970-01-01");
        assert_eq!(serial_to_iso(45_360.0), "2024-03-09");
        assert_eq!(serial_to_iso(45_360.5), "2024-03-09 12:00:00");
        assert_eq!(serial_to_iso(45_360.999_999_9), "2024-03-10");
        // Before Excel's fictitious 1900-02-29 the epoch is one day earlier.
        assert_eq!(serial_to_iso(1.0), "1900-01-01");
        assert_eq!(serial_to_iso(59.0), "1900-02-28");
        assert_eq!(serial_to_iso(61.0), "1900-03-01");
    }

    #[test]
    fn too_long_text_is_an_error_not_a_silent_truncation() {
        let mut writer = SheetWriter::new(&["t".into()]).unwrap();
        let err = writer
            .write_row(&[Value::Text("x".repeat(MAX_CELL_CHARS + 1))])
            .unwrap_err();
        assert!(err.to_string().contains("32 767"), "{err}");
    }

    #[test]
    fn large_integers_are_written_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.xlsx");
        let mut writer = SheetWriter::new(&["n".into()]).unwrap();
        writer
            .write_row(&[Value::Int(9_007_199_254_740_993)])
            .unwrap();
        writer.save(&path).unwrap();
        let (names, rows) = read_sheet(&path, None, true).unwrap();
        assert_eq!(names, ["n"]);
        assert_eq!(rows[0][0], Value::Text("9007199254740993".into()));
    }
}
