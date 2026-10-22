//! The rows the grid shows, in order (§7.5): a permutation of the loaded rows (sorted
//! locally when the result is complete), then the inserted rows.

use std::cmp::Ordering;

use sqlr_core::{Cell, ColumnMeta, DisplayHint, RowBuffer, compare_cells};

/// What a computed view depends on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewKey {
    pub loaded: usize,
    /// The inserted rows' numbers, shown after the loaded rows.
    pub inserted: Vec<usize>,
    pub sort: Vec<(usize, bool)>,
    /// Columns' identity (names and types), so a re-run result is recomputed.
    pub columns: u64,
}

/// Next state of a header click: ascending → descending → none.
pub(crate) fn cycle(sort: &[(usize, bool)], col: usize) -> Vec<(usize, bool)> {
    match sort {
        [(c, false)] if *c == col => vec![(col, true)],
        [(c, true)] if *c == col => Vec::new(),
        _ => vec![(col, false)],
    }
}

/// Typed comparison of two non-NULL cells. Exact decimals carried as text compare as
/// numbers (`total_cmp`); in such a column, text that is not a number sorts after every
/// number, among itself byte-wise, so the order stays total.
fn compare_values(meta: &ColumnMeta, a: Cell<'_>, b: Cell<'_>) -> Ordering {
    if matches!(meta.hint, DisplayHint::Decimal | DisplayHint::Number) {
        let number = |c: Cell<'_>| match c {
            Cell::Text(t) => t.trim().parse::<f64>().ok(),
            _ => None,
        };
        match (a, b) {
            (Cell::Text(_), Cell::Text(_)) => {
                return match (number(a), number(b)) {
                    (Some(x), Some(y)) => x.total_cmp(&y),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => compare_cells(a, b),
                };
            }
            _ => return compare_cells(a, b),
        }
    }
    compare_cells(a, b)
}

/// Rows of `rows` shown, in order. `sort` applies only when `local_sort`; NULLs sort last
/// either way, and the sort is stable.
pub(crate) fn compute(
    rows: &RowBuffer,
    sort: &[(usize, bool)],
    local_sort: bool,
    inserted: &[usize],
) -> Vec<usize> {
    let loaded = rows.row_count();
    let columns = rows.columns();
    let mut view: Vec<usize> = (0..loaded).collect();
    let keys: Vec<(usize, bool)> = sort
        .iter()
        .copied()
        .filter(|(c, _)| *c < columns.len())
        .collect();
    if local_sort && !keys.is_empty() {
        view.sort_by(|&a, &b| {
            for &(c, desc) in &keys {
                let (x, y) = (
                    rows.cell(a, c).unwrap_or(Cell::Null),
                    rows.cell(b, c).unwrap_or(Cell::Null),
                );
                let ord = match (x == Cell::Null, y == Cell::Null) {
                    (true, true) => Ordering::Equal,
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (false, false) => {
                        let ord = compare_values(&columns[c], x, y);
                        if desc { ord.reverse() } else { ord }
                    }
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            Ordering::Equal
        });
    }
    view.extend(inserted.iter().copied());
    view
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlr_core::{ChunkBuilder, ColumnKind};

    use super::*;

    #[test]
    fn decimals_sort_as_numbers_and_header_clicks_cycle() {
        let cols: Arc<[ColumnMeta]> = Arc::from(vec![ColumnMeta::new(
            "d",
            "numeric",
            ColumnKind::Text,
            DisplayHint::Decimal,
        )]);
        let mut buf = RowBuffer::new(cols.clone());
        let mut b = ChunkBuilder::new(cols);
        for v in ["10.5", "9", "100"] {
            b.push_row(&[Cell::Text(v)]).unwrap();
        }
        buf.push_chunk(b.finish());
        assert_eq!(compute(&buf, &[(0, false)], true, &[7]), [1, 0, 2, 7]);
        assert_eq!(compute(&buf, &[(0, false)], false, &[]), [0, 1, 2]);
        assert_eq!(cycle(&[], 2), [(2, false)]);
        assert_eq!(cycle(&[(2, false)], 2), [(2, true)]);
        assert!(cycle(&[(2, true)], 2).is_empty());
        assert_eq!(cycle(&[(1, true)], 2), [(2, false)]);
    }

    #[test]
    fn decimals_order_numbers_before_other_text_consistently() {
        let cols: Arc<[ColumnMeta]> = Arc::from(vec![ColumnMeta::new(
            "d",
            "numeric",
            ColumnKind::Text,
            DisplayHint::Decimal,
        )]);
        let mut buf = RowBuffer::new(cols.clone());
        let mut b = ChunkBuilder::new(cols);
        for v in ["b", "10", "NaN", "a", "9", "-1"] {
            b.push_row(&[Cell::Text(v)]).unwrap();
        }
        buf.push_chunk(b.finish());
        // -1, 9, 10, NaN (numbers, total order), then text: a, b.
        assert_eq!(compute(&buf, &[(0, false)], true, &[]), [5, 4, 1, 2, 3, 0]);
        assert_eq!(compute(&buf, &[(0, true)], true, &[]), [0, 3, 2, 1, 4, 5]);
    }
}
