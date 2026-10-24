//! The rows the grid shows, in order (§7.5): a permutation of the loaded rows (sorted
//! locally when the result is complete), then the inserted rows.

use std::cmp::Ordering;

use sqlr_core::{Cell, ColumnMeta, DisplayHint, RowBuffer, compare_cells};

/// What a computed view depends on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewKey {
    pub loaded: usize,
    pub revision: u64,
    /// The inserted rows' numbers, shown after the loaded rows.
    pub inserted: Vec<usize>,
    pub sort: Vec<(usize, bool)>,
    /// Columns' identity (names and types), so a re-run result is recomputed.
    pub columns: u64,
}

/// The natural order needs only a count; an explicit permutation exists only after sorting.
pub(crate) enum RowOrder {
    Natural { loaded: usize, inserted: Vec<usize> },
    Sorted(Vec<usize>),
}
impl RowOrder {
    pub fn len(&self) -> usize {
        match self {
            Self::Natural { loaded, inserted } => loaded + inserted.len(),
            Self::Sorted(v) => v.len(),
        }
    }
    pub fn get(&self, i: usize) -> Option<usize> {
        match self {
            Self::Natural { loaded, inserted } => {
                if i < *loaded {
                    Some(i)
                } else {
                    inserted.get(i - loaded).copied()
                }
            }
            Self::Sorted(v) => v.get(i).copied(),
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len()).map(|i| self.get(i).expect("view row"))
    }
    pub fn position(&self, source: usize) -> Option<usize> {
        match self {
            Self::Natural { loaded, inserted } => {
                if source < *loaded {
                    Some(source)
                } else {
                    inserted
                        .iter()
                        .position(|&r| r == source)
                        .map(|i| loaded + i)
                }
            }
            Self::Sorted(v) => v.iter().position(|&r| r == source),
        }
    }
    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        match self {
            Self::Natural { inserted, .. } => inserted.capacity(),
            Self::Sorted(v) => v.capacity(),
        }
    }
}

/// Next state of a header click: ascending → descending → none.
pub(crate) fn cycle(sort: &[(usize, bool)], col: usize) -> Vec<(usize, bool)> {
    match sort {
        [(c, false)] if *c == col => vec![(col, true)],
        [(c, true)] if *c == col => Vec::new(),
        _ => vec![(col, false)],
    }
}

/// An exact decimal text as `(negative, significant digits, power of ten)`: the value is
/// `digits * 10^exp`. `None` for text that is not a plain decimal (NaN, infinity, …).
fn parse_decimal(text: &str) -> Option<(bool, String, i128)> {
    let text = text.trim();
    let (neg, rest) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let (mantissa, exponent) = match rest.find(['e', 'E']) {
        Some(at) => (&rest[..at], rest[at + 1..].parse::<i128>().ok()?),
        None => (rest, 0),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if (int.is_empty() && frac.is_empty())
        || !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut digits = format!("{int}{frac}");
    let mut exp = exponent - frac.len() as i128;
    let first = digits.find(|c: char| c != '0');
    let Some(first) = first else {
        return Some((false, String::new(), 0)); // zero
    };
    digits.drain(..first);
    while digits.ends_with('0') {
        digits.pop();
        exp += 1;
    }
    Some((neg, digits, exp))
}

/// Exact comparison of two decimal texts. The `f64` fast path in [`compare_values`] can
/// tie on values that differ beyond its precision (large integers, long fractions), so a
/// tie is settled here.
fn compare_decimals(a: &str, b: &str) -> Ordering {
    let (Some((neg_a, digits_a, exp_a)), Some((neg_b, digits_b, exp_b))) =
        (parse_decimal(a), parse_decimal(b))
    else {
        return compare_cells(Cell::Text(a), Cell::Text(b));
    };
    compare_parsed_decimals(&(neg_a, digits_a, exp_a), &(neg_b, digits_b, exp_b))
}

fn compare_parsed_decimals(
    (neg_a, digits_a, exp_a): &(bool, String, i128),
    (neg_b, digits_b, exp_b): &(bool, String, i128),
) -> Ordering {
    let zero_a = digits_a.is_empty();
    let zero_b = digits_b.is_empty();
    match (zero_a, zero_b) {
        (true, true) => return Ordering::Equal,
        (true, false) => {
            return if *neg_b {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        (false, true) => {
            return if *neg_a {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        (false, false) => {}
    }
    if neg_a != neg_b {
        return if *neg_a {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    // The decimal point's position relative to the significant digits: larger is larger.
    let magnitude_a = digits_a.len() as i128 + *exp_a;
    let magnitude_b = digits_b.len() as i128 + *exp_b;
    let ordinal = magnitude_a.cmp(&magnitude_b).then_with(|| {
        let len = digits_a.len().max(digits_b.len());
        let digit = |digits: &str, i: usize| digits.as_bytes().get(i).copied().unwrap_or(b'0');
        (0..len)
            .map(|i| digit(digits_a, i).cmp(&digit(digits_b, i)))
            .find(|o| *o != Ordering::Equal)
            .unwrap_or(Ordering::Equal)
    });
    if *neg_a { ordinal.reverse() } else { ordinal }
}

/// Typed comparison of two non-NULL cells. Exact decimals carried as text compare as
/// numbers (`total_cmp`, then exactly); in such a column, text that is not a number sorts
/// after every number, among itself byte-wise, so the order stays total.
fn compare_values(meta: &ColumnMeta, a: Cell<'_>, b: Cell<'_>) -> Ordering {
    if matches!(meta.hint, DisplayHint::Decimal | DisplayHint::Number) {
        let number = |c: Cell<'_>| match c {
            Cell::Text(t) => t.trim().parse::<f64>().ok(),
            _ => None,
        };
        match (a, b) {
            (Cell::Text(x), Cell::Text(y)) => {
                return match (number(a), number(b)) {
                    (Some(fx), Some(fy)) => fx.total_cmp(&fy).then_with(|| compare_decimals(x, y)),
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
    // Typed columns need no numeric-text cache. Keep their direct comparison path
    // small: adding cache dispatch to every comparison penalizes large integer sorts.
    if local_sort
        && !keys.is_empty()
        && !keys.iter().any(|&(c, _)| {
            columns[c].kind == sqlr_core::ColumnKind::Text
                && matches!(columns[c].hint, DisplayHint::Decimal | DisplayHint::Number)
        })
    {
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
        view.extend(inserted.iter().copied());
        return view;
    }
    if local_sort && !keys.is_empty() {
        // Parse numeric text once per row. Ordinary typed columns borrow their values
        // directly so sorting integers does not allocate a large per-cell key cache.
        struct NumericKey {
            number: Option<f64>,
            decimal: Option<(bool, String, i128)>,
            null: bool,
        }
        let prepared: Vec<_> = keys
            .iter()
            .map(|&(c, desc)| {
                let values = (columns[c].kind == sqlr_core::ColumnKind::Text
                    && matches!(columns[c].hint, DisplayHint::Decimal | DisplayHint::Number))
                .then(|| {
                    (0..loaded)
                        .map(|r| {
                            let cell = rows.cell(r, c).unwrap_or(Cell::Null);
                            let (number, decimal) = match cell {
                                Cell::Text(t) => (t.trim().parse::<f64>().ok(), parse_decimal(t)),
                                _ => (None, None),
                            };
                            NumericKey {
                                number,
                                decimal,
                                null: cell == Cell::Null,
                            }
                        })
                        .collect::<Vec<_>>()
                });
                (c, desc, values)
            })
            .collect();
        view.sort_by(|&a, &b| {
            for (c, desc, values) in &prepared {
                let cells = values.is_none().then(|| {
                    (
                        rows.cell(a, *c).unwrap_or(Cell::Null),
                        rows.cell(b, *c).unwrap_or(Cell::Null),
                    )
                });
                let compare = || {
                    let (x, y) = cells.unwrap_or_else(|| {
                        (
                            rows.cell(a, *c).unwrap_or(Cell::Null),
                            rows.cell(b, *c).unwrap_or(Cell::Null),
                        )
                    });
                    compare_values(&columns[*c], x, y)
                };
                let (null_a, null_b) = if let Some(v) = values {
                    (v[a].null, v[b].null)
                } else {
                    let (x, y) = cells.unwrap();
                    (x == Cell::Null, y == Cell::Null)
                };
                let ord = match (null_a, null_b) {
                    (true, true) => Ordering::Equal,
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (false, false) => {
                        let ord = if let Some(v) = values {
                            let (x, y) = (&v[a], &v[b]);
                            match (x.number, y.number) {
                                (Some(fx), Some(fy)) => {
                                    fx.total_cmp(&fy)
                                        .then_with(|| match (&x.decimal, &y.decimal) {
                                            (Some(dx), Some(dy)) => compare_parsed_decimals(dx, dy),
                                            _ => compare(),
                                        })
                                }
                                (Some(_), None) => Ordering::Less,
                                (None, Some(_)) => Ordering::Greater,
                                (None, None) => compare(),
                            }
                        } else {
                            compare()
                        };
                        if *desc { ord.reverse() } else { ord }
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
    fn decimals_beyond_f64_precision_sort_exactly() {
        let cols: Arc<[ColumnMeta]> = Arc::from(vec![ColumnMeta::new(
            "d",
            "numeric",
            ColumnKind::Text,
            DisplayHint::Decimal,
        )]);
        let mut buf = RowBuffer::new(cols.clone());
        let mut b = ChunkBuilder::new(cols);
        // The first two are the same f64; only an exact comparison tells them apart.
        for v in [
            "9007199254740993",
            "9007199254740992",
            "1.0000000000000002",
            "1.0000000000000001",
            "0",
            "-0.5",
        ] {
            b.push_row(&[Cell::Text(v)]).unwrap();
        }
        buf.push_chunk(b.finish());
        assert_eq!(
            compute(&buf, &[(0, false)], true, &[]),
            [5, 4, 3, 2, 1, 0],
            "negative, zero, fractions, then large integers"
        );
        assert_eq!(compute(&buf, &[(0, true)], true, &[]), [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn decimals_in_exponent_form_compare_exactly() {
        assert_eq!(
            compare_decimals("1e20", "100000000000000000000"),
            Ordering::Equal
        );
        assert_eq!(compare_decimals("1.2e3", "1200"), Ordering::Equal);
        assert_eq!(compare_decimals("1.2e3", "1201"), Ordering::Less);
        assert_eq!(compare_decimals("-1e20", "-1e21"), Ordering::Greater);
        assert_eq!(compare_decimals("0.000", "-0"), Ordering::Equal);
        assert_eq!(compare_decimals("0", "-0.1"), Ordering::Greater);
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
