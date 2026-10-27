//! Typed, masked clipboard data shared by the grid and background clipboard jobs.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use crate::{Cell, ColumnMeta, Dialect, Encoder, RowBuffer, TextFormat, Value};

/// Why clipboard data cannot be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardError {
    /// A pushed row has another width than the clipboard's columns.
    #[error("Clipboard row width does not match its columns")]
    RowWidth,
    /// The data and its text exceed [`MAX_CLIPBOARD_BYTES`].
    #[error("The clipboard exceeds 128 MiB; export the results instead")]
    TooLarge,
    /// A selected column no longer exists in the result.
    #[error("Clipboard column no longer exists")]
    ColumnGone,
}

/// Text plus owned cell values. `None` is an unselected hole, distinct from SQL NULL.
#[derive(Clone, Debug, PartialEq)]
pub struct GridClipboard {
    /// Metadata of the copied columns.
    pub columns: Vec<ColumnMeta>,
    /// The copied values, one entry per row; `None` marks an unselected cell.
    pub rows: Vec<Vec<Option<Value>>>,
    /// The same data encoded as text in the chosen format.
    pub text: String,
}

/// Clipboard data and its text are bounded together; larger transfers should use export.
pub const MAX_CLIPBOARD_BYTES: usize = 128 * 1024 * 1024;

/// A rectangular selection of the grid, in view coordinates.
#[derive(Clone, Debug)]
pub struct ClipboardRegion {
    /// The selected view rows.
    pub rows: RangeInclusive<usize>,
    /// The selected columns.
    pub cols: RangeInclusive<usize>,
}

/// Immutable result snapshot, selection in view order, and pending values. Cloning result
/// chunks only clones their Arcs, so the UI can hand encoding to a background job.
#[derive(Clone, Debug)]
pub struct ClipboardSource {
    /// The result rows the selection reads from.
    pub rows: RowBuffer,
    /// (view row, source row), including pending inserted rows.
    pub order: Vec<(usize, usize)>,
    /// The selected source columns, in selection order.
    pub cols: Vec<usize>,
    /// The selected rectangles.
    pub regions: Vec<ClipboardRegion>,
    /// Pending cell edits by (view row, column), which replace the stored value.
    pub overrides: BTreeMap<(usize, usize), Value>,
}

/// Builds a `GridClipboard` row by row, enforcing `MAX_CLIPBOARD_BYTES`.
pub struct ClipboardBuilder<'d> {
    data: GridClipboard,
    encoder: Encoder<'d>,
    value_bytes: usize,
}

impl<'d> ClipboardBuilder<'d> {
    /// Starts a clipboard with `columns` and writes the header for `format`.
    pub fn new(
        columns: Vec<ColumnMeta>,
        format: TextFormat,
        dialect: Option<&'d dyn Dialect>,
    ) -> Self {
        let encoder = Encoder::new(format, &columns, dialect).with_spreadsheet_safe();
        let mut data = GridClipboard {
            columns,
            rows: Vec::new(),
            text: String::new(),
        };
        encoder.header(&mut data.text);
        Self {
            data,
            encoder,
            value_bytes: 0,
        }
    }

    /// Appends a row, failing when its width is wrong or the clipboard exceeds `MAX_CLIPBOARD_BYTES`.
    pub fn push(&mut self, row: Vec<Option<Value>>) -> Result<(), ClipboardError> {
        if row.len() != self.data.columns.len() {
            return Err(ClipboardError::RowWidth);
        }
        self.value_bytes += std::mem::size_of::<Vec<Option<Value>>>()
            + row.len() * std::mem::size_of::<Option<Value>>()
            + row
                .iter()
                .flatten()
                .map(|v| match v {
                    Value::Text(t) => t.len(),
                    Value::Bytes(b) => b.len(),
                    _ => 0,
                })
                .sum::<usize>();
        let cells: Vec<_> = row
            .iter()
            .map(|v| v.as_ref().map_or(Cell::Null, Value::as_cell))
            .collect();
        self.encoder.row(&cells, &mut self.data.text);
        if self.value_bytes.saturating_add(self.data.text.len()) > MAX_CLIPBOARD_BYTES {
            return Err(ClipboardError::TooLarge);
        }
        self.data.rows.push(row);
        Ok(())
    }

    /// The number of rows pushed so far.
    pub fn rows(&self) -> usize {
        self.data.rows.len()
    }
    /// The size of the encoded text so far, in bytes.
    pub fn bytes(&self) -> usize {
        self.data.text.len()
    }

    /// Writes the footer and returns the finished clipboard.
    pub fn finish(mut self) -> GridClipboard {
        self.encoder.footer(&mut self.data.text);
        self.data
    }
}

impl ClipboardSource {
    /// `progress` returns false to cancel, without publishing a partial clipboard.
    pub fn encode(
        &self,
        format: TextFormat,
        dialect: Option<&dyn Dialect>,
        mut progress: impl FnMut(usize, usize) -> bool,
    ) -> Result<Option<GridClipboard>, ClipboardError> {
        let columns = self
            .cols
            .iter()
            .map(|&c| {
                self.rows
                    .columns()
                    .get(c)
                    .cloned()
                    .ok_or(ClipboardError::ColumnGone)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut builder = ClipboardBuilder::new(columns, format, dialect);
        for &(view, source) in &self.order {
            if !progress(builder.rows(), builder.bytes()) {
                return Ok(None);
            }
            let row = self
                .cols
                .iter()
                .map(|&col| {
                    let selected = self
                        .regions
                        .iter()
                        .any(|r| r.rows.contains(&view) && r.cols.contains(&col));
                    selected.then(|| {
                        self.overrides
                            .get(&(source, col))
                            .cloned()
                            .or_else(|| self.rows.cell(source, col).map(Value::from_cell))
                            .unwrap_or(Value::Null)
                    })
                })
                .collect();
            builder.push(row)?;
        }
        progress(builder.rows(), builder.bytes())
            .then(|| builder.finish())
            .map_or(Ok(None), |v| Ok(Some(v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChunkBuilder, ColumnKind, CsvOptions, DisplayHint};

    #[test]
    fn clipboard_errors_keep_their_messages() {
        assert_eq!(
            ClipboardError::RowWidth.to_string(),
            "Clipboard row width does not match its columns"
        );
        assert_eq!(
            ClipboardError::TooLarge.to_string(),
            "The clipboard exceeds 128 MiB; export the results instead"
        );
        assert_eq!(
            ClipboardError::ColumnGone.to_string(),
            "Clipboard column no longer exists"
        );
    }

    #[test]
    fn selection_encoding_keeps_holes_pending_values_and_view_order() {
        let columns: std::sync::Arc<[ColumnMeta]> = vec![
            ColumnMeta::new("id", "int", ColumnKind::Int, DisplayHint::Number),
            ColumnMeta::new("name", "text", ColumnKind::Text, DisplayHint::Plain),
        ]
        .into();
        let mut rows = RowBuffer::new(columns.clone());
        let mut b = ChunkBuilder::new(columns);
        b.push_row(&[Cell::Int(1), Cell::Text("")]).unwrap();
        b.push_row(&[Cell::Int(2), Cell::Null]).unwrap();
        rows.push_chunk(b.finish());
        let source = ClipboardSource {
            rows,
            order: vec![(0, 1), (1, 0)],
            cols: vec![0, 1],
            regions: vec![
                ClipboardRegion {
                    rows: 0..=0,
                    cols: 0..=0,
                },
                ClipboardRegion {
                    rows: 1..=1,
                    cols: 1..=1,
                },
            ],
            overrides: [((0, 1), Value::Text("changed".into()))].into(),
        };
        let data = source
            .encode(TextFormat::Csv(CsvOptions::tsv()), None, |_, _| true)
            .unwrap()
            .unwrap();
        assert_eq!(
            data.rows,
            vec![
                vec![Some(Value::Int(2)), None],
                vec![None, Some(Value::Text("changed".into()))]
            ]
        );
        assert!(
            source
                .encode(TextFormat::Tsv, None, |_, _| false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn builder_preserves_null_empty_text_bytes_and_spreadsheet_formula_text() {
        let columns = vec![ColumnMeta::new(
            "v",
            "mixed",
            ColumnKind::Mixed,
            DisplayHint::Plain,
        )];
        let mut builder = ClipboardBuilder::new(columns, TextFormat::Csv(CsvOptions::tsv()), None);
        let values = [
            Value::Null,
            Value::Text(String::new()),
            Value::Bytes(vec![0, 255]),
            Value::Text("=1+1".into()),
        ];
        for value in &values {
            builder.push(vec![Some(value.clone())]).unwrap();
        }
        let data = builder.finish();
        assert_eq!(
            data.rows,
            values
                .into_iter()
                .map(|v| vec![Some(v)])
                .collect::<Vec<_>>()
        );
        assert!(data.text.contains("\"\""));
        assert!(data.text.contains("'=1+1"));
        assert!(data.text.contains("0x00ff"));
    }
}
