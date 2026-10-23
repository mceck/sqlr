//! The grid's rectangular selection (in view coordinates) and copying it as text.

use std::ops::RangeInclusive;

use sqlr_core::{Cell, ColumnMeta, CsvOptions, Dialect, Encoder, TextFormat, Value};

/// A rectangle of cells between `anchor` (where it started) and `cursor` (the cell the
/// keyboard moves), as `(view row, column)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, usize),
    pub cursor: (usize, usize),
}

impl Selection {
    pub fn cell(row: usize, col: usize) -> Self {
        Self {
            anchor: (row, col),
            cursor: (row, col),
        }
    }

    pub fn rows(&self) -> RangeInclusive<usize> {
        self.anchor.0.min(self.cursor.0)..=self.anchor.0.max(self.cursor.0)
    }

    pub fn cols(&self) -> RangeInclusive<usize> {
        self.anchor.1.min(self.cursor.1)..=self.anchor.1.max(self.cursor.1)
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        self.rows().contains(&row) && self.cols().contains(&col)
    }

    pub fn cell_count(&self) -> usize {
        self.rows().count() * self.cols().count()
    }

    /// Keeps both corners inside `rows` × `cols` (both non-zero).
    pub(crate) fn clamp(&mut self, rows: usize, cols: usize) {
        let fit = |(r, c): (usize, usize)| (r.min(rows - 1), c.min(cols - 1));
        self.anchor = fit(self.anchor);
        self.cursor = fit(self.cursor);
    }
}

/// `selection` without `cut`, as up to four rectangles.
pub(crate) fn subtract(selection: &Selection, cut: &Selection) -> Vec<Selection> {
    let (r0, r1) = (*selection.rows().start(), *selection.rows().end());
    let (c0, c1) = (*selection.cols().start(), *selection.cols().end());
    let (cr0, cr1) = (*cut.rows().start(), *cut.rows().end());
    let (cc0, cc1) = (*cut.cols().start(), *cut.cols().end());
    if cr1 < r0 || cr0 > r1 || cc1 < c0 || cc0 > c1 {
        return vec![*selection];
    }
    let mut out = Vec::new();
    if cr0 > r0 {
        out.push(rect(r0, cr0 - 1, c0, c1));
    }
    if cr1 < r1 {
        out.push(rect(cr1 + 1, r1, c0, c1));
    }
    let mid_r0 = cr0.max(r0);
    let mid_r1 = cr1.min(r1);
    if mid_r0 <= mid_r1 {
        if cc0 > c0 {
            out.push(rect(mid_r0, mid_r1, c0, cc0 - 1));
        }
        if cc1 < c1 {
            out.push(rect(mid_r0, mid_r1, cc1 + 1, c1));
        }
    }
    out
}

fn rect(r0: usize, r1: usize, c0: usize, c1: usize) -> Selection {
    Selection {
        anchor: (r0, c0),
        cursor: (r1, c1),
    }
}

/// The text formats of "Copy" (TSV) and "Copy as ▸".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyFormat {
    Tsv,
    Csv,
    Json,
    SqlInsert,
    Markdown,
}

impl CopyFormat {
    pub const ALL: [CopyFormat; 5] = [
        CopyFormat::Tsv,
        CopyFormat::Csv,
        CopyFormat::Json,
        CopyFormat::SqlInsert,
        CopyFormat::Markdown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CopyFormat::Tsv => "TSV",
            CopyFormat::Csv => "CSV",
            CopyFormat::Json => "JSON",
            CopyFormat::SqlInsert => "SQL INSERT",
            CopyFormat::Markdown => "Markdown",
        }
    }

    /// The encoder format. TSV names the columns only with `header` (whole columns
    /// selected); CSV always does; SQL `INSERT`s go into `table` (`"result"` without one).
    pub fn text_format(self, header: bool, table: Option<&str>) -> TextFormat {
        match self {
            CopyFormat::Tsv => TextFormat::Csv(CsvOptions {
                header,
                ..CsvOptions::tsv()
            }),
            CopyFormat::Csv => TextFormat::Csv(CsvOptions::default()),
            CopyFormat::Json => TextFormat::Json,
            CopyFormat::SqlInsert => TextFormat::SqlInsert {
                table: table.unwrap_or("result").to_string(),
            },
            CopyFormat::Markdown => TextFormat::Markdown,
        }
    }
}

/// `rows` (each a row's values for `columns`) encoded in `format`.
pub(crate) fn encode(
    format: TextFormat,
    columns: &[ColumnMeta],
    dialect: Option<&dyn Dialect>,
    rows: impl Iterator<Item = Vec<Value>>,
) -> String {
    let mut encoder = Encoder::new(format, columns, dialect).with_spreadsheet_safe();
    let mut out = String::new();
    encoder.header(&mut out);
    for row in rows {
        let cells: Vec<Cell<'_>> = row.iter().map(Value::as_cell).collect();
        encoder.row(&cells, &mut out);
    }
    encoder.footer(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(r0: usize, r1: usize, c0: usize, c1: usize) -> Selection {
        Selection {
            anchor: (r0, c0),
            cursor: (r1, c1),
        }
    }

    #[test]
    fn subtracting_a_cell_splits_the_rectangle() {
        assert_eq!(
            subtract(&sel(0, 3, 0, 3), &Selection::cell(1, 1)),
            [
                sel(0, 0, 0, 3),
                sel(2, 3, 0, 3),
                sel(1, 1, 0, 0),
                sel(1, 1, 2, 3)
            ]
        );
    }

    #[test]
    fn subtracting_a_row_leaves_the_rows_around_it() {
        assert_eq!(
            subtract(&sel(0, 3, 0, 3), &sel(1, 1, 0, 3)),
            [sel(0, 0, 0, 3), sel(2, 3, 0, 3)]
        );
    }

    #[test]
    fn subtracting_a_disjoint_or_covering_rectangle() {
        assert_eq!(
            subtract(&sel(0, 1, 0, 1), &sel(2, 3, 2, 3)),
            [sel(0, 1, 0, 1)]
        );
        assert!(subtract(&sel(0, 1, 0, 1), &sel(0, 1, 0, 1)).is_empty());
    }
}
