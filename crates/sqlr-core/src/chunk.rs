//! Columnar, immutable chunk of result rows (spec §7.2).

use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

use crate::table::TableRef;

mod spill;
pub use spill::{SpillBacking, SpillEncoding};

/// The physical kind of a column: how its values are stored in a chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ColumnKind {
    /// Booleans.
    Bool,
    /// 64-bit signed integers.
    Int,
    /// 64-bit floats.
    Float,
    /// UTF-8 text (offsets over a string arena).
    Text,
    /// Raw bytes (offsets over a byte arena).
    Bytes,
    /// Any cell, each stored with its own type (SQLite's dynamic typing).
    Mixed,
}

/// How a column's values are shown; the physical kind stays one of [`ColumnKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DisplayHint {
    /// Shown as the value reads.
    Plain,
    /// A number, right-aligned.
    Number,
    /// Exact numbers carried as text (`numeric`, `DECIMAL`).
    Decimal,
    /// A date, time or timestamp.
    Temporal,
    /// A time interval.
    Interval,
    /// A JSON document.
    Json,
    /// An XML document.
    Xml,
    /// A UUID.
    Uuid,
    /// An array value.
    Array,
    /// A geometry or geography value.
    Geometry,
    /// Binary data, shown in hex.
    Binary,
}

/// The table column a result column was read from, when the server reports it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ColumnOrigin {
    /// The table the column belongs to.
    pub table: TableRef,
    /// The column's name in that table.
    pub column: String,
}

/// Name, type and display properties of one result column.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColumnMeta {
    /// Column name or alias as the result names it.
    pub name: String,
    /// Type name as the server reports it.
    pub type_name: String,
    /// How values are stored in a chunk.
    pub kind: ColumnKind,
    /// How values are shown.
    pub hint: DisplayHint,
    /// The table column it was read from, when known.
    pub origin: Option<ColumnOrigin>,
}

impl ColumnMeta {
    /// A column without an origin.
    pub fn new(
        name: impl Into<String>,
        type_name: impl Into<String>,
        kind: ColumnKind,
        hint: DisplayHint,
    ) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
            kind,
            hint,
            origin: None,
        }
    }

    /// The column with `origin` set.
    pub fn with_origin(mut self, origin: Option<ColumnOrigin>) -> Self {
        self.origin = origin;
        self
    }
}

/// A borrowed view of one cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cell<'a> {
    /// SQL `NULL`.
    Null,
    /// A boolean.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A float.
    Float(f64),
    /// Text.
    Text(&'a str),
    /// Bytes.
    Bytes(&'a [u8]),
}

/// `0x` followed by the lowercase hex digits of `bytes`.
pub fn hex_bytes(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0x0f)] as char);
    }
    out
}

/// Total order over cells of any kind, SQLite's rule: NULL < numbers (booleans as 0/1;
/// integers and floats compared numerically, NaN lowest) < text (byte-wise) < bytes.
pub fn compare_cells(a: Cell<'_>, b: Cell<'_>) -> Ordering {
    fn class(c: &Cell<'_>) -> u8 {
        match c {
            Cell::Null => 0,
            Cell::Bool(_) | Cell::Int(_) | Cell::Float(_) => 1,
            Cell::Text(_) => 2,
            Cell::Bytes(_) => 3,
        }
    }
    fn number(c: Cell<'_>) -> Result<i64, f64> {
        match c {
            Cell::Bool(b) => Ok(i64::from(b)),
            Cell::Int(i) => Ok(i),
            Cell::Float(f) => Err(f),
            _ => Ok(0),
        }
    }
    fn floats(x: f64, y: f64) -> Ordering {
        match (x.is_nan(), y.is_nan()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        }
    }
    /// Exact, without rounding `i` to a float.
    fn int_float(i: i64, f: f64) -> Ordering {
        const TWO_63: f64 = 9_223_372_036_854_775_808.0;
        if f.is_nan() || f < -TWO_63 {
            return Ordering::Greater;
        }
        if f >= TWO_63 {
            return Ordering::Less;
        }
        let whole = f.trunc();
        i.cmp(&(whole as i64))
            .then_with(|| 0.0.partial_cmp(&(f - whole)).unwrap_or(Ordering::Equal))
    }
    class(&a).cmp(&class(&b)).then_with(|| match (a, b) {
        (Cell::Text(x), Cell::Text(y)) => x.as_bytes().cmp(y.as_bytes()),
        (Cell::Bytes(x), Cell::Bytes(y)) => x.cmp(y),
        (Cell::Null, Cell::Null) => Ordering::Equal,
        _ => match (number(a), number(b)) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Err(x), Err(y)) => floats(x, y),
            (Ok(x), Err(y)) => int_float(x, y),
            (Err(x), Ok(y)) => int_float(y, x).reverse(),
        },
    })
}

impl fmt::Display for Cell<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cell::Null => f.write_str("NULL"),
            Cell::Bool(v) => write!(f, "{v}"),
            Cell::Int(v) => write!(f, "{v}"),
            Cell::Float(v) => write!(f, "{v}"),
            Cell::Text(v) => f.write_str(v),
            Cell::Bytes(v) => f.write_str(&hex_bytes(v)),
        }
    }
}

/// Why a row cannot be pushed to a [`ChunkBuilder`].
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ChunkError {
    /// The row has a different number of cells than the chunk has columns.
    #[error("row has {got} cells, expected {expected}")]
    Arity {
        /// Number of columns.
        expected: usize,
        /// Number of cells in the row.
        got: usize,
    },
    /// A cell does not fit the kind of its column.
    #[error("cell {col} does not match column kind {kind:?}")]
    KindMismatch {
        /// Index of the cell.
        col: usize,
        /// The kind the column stores.
        kind: ColumnKind,
    },
}

#[derive(Clone, Debug)]
enum ColumnData {
    Bool(Vec<bool>),
    Int(Vec<i64>),
    Float(Vec<f64>),
    /// `offsets` has `len + 1` entries into `arena`, which holds only whole `&str` values.
    Text {
        offsets: Vec<u32>,
        arena: Vec<u8>,
    },
    /// Like `Text`, for arbitrary bytes.
    Bytes {
        offsets: Vec<u32>,
        arena: Vec<u8>,
    },
    /// Per row: a storage tag, the number bits (bool, i64 or f64) and an arena slice for
    /// text and bytes.
    Mixed {
        tags: Vec<MixedTag>,
        numbers: Vec<u64>,
        offsets: Vec<u32>,
        arena: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MixedTag {
    Null,
    Bool,
    Int,
    Float,
    Text,
    Bytes,
}

impl ColumnData {
    fn new(kind: ColumnKind) -> Self {
        match kind {
            ColumnKind::Bool => ColumnData::Bool(Vec::new()),
            ColumnKind::Int => ColumnData::Int(Vec::new()),
            ColumnKind::Float => ColumnData::Float(Vec::new()),
            ColumnKind::Text => ColumnData::Text {
                offsets: vec![0],
                arena: Vec::new(),
            },
            ColumnKind::Bytes => ColumnData::Bytes {
                offsets: vec![0],
                arena: Vec::new(),
            },
            ColumnKind::Mixed => ColumnData::Mixed {
                tags: Vec::new(),
                numbers: Vec::new(),
                offsets: vec![0],
                arena: Vec::new(),
            },
        }
    }

    /// Appends a value; `None` pushes a placeholder (validity is tracked separately).
    fn push(&mut self, cell: Option<Cell<'_>>) {
        match (self, cell) {
            (ColumnData::Bool(v), Some(Cell::Bool(x))) => v.push(x),
            (ColumnData::Bool(v), None) => v.push(false),
            (ColumnData::Int(v), Some(Cell::Int(x))) => v.push(x),
            (ColumnData::Int(v), None) => v.push(0),
            (ColumnData::Float(v), Some(Cell::Float(x))) => v.push(x),
            (ColumnData::Float(v), None) => v.push(0.0),
            (ColumnData::Text { offsets, arena }, c) => {
                if let Some(Cell::Text(s)) = c {
                    arena.extend_from_slice(s.as_bytes());
                }
                let end = u32::try_from(arena.len()).expect("text arena exceeds 4 GiB");
                offsets.push(end);
            }
            (
                ColumnData::Mixed {
                    tags,
                    numbers,
                    offsets,
                    arena,
                },
                c,
            ) => {
                let (tag, number) = match c {
                    None | Some(Cell::Null) => (MixedTag::Null, 0),
                    Some(Cell::Bool(b)) => (MixedTag::Bool, u64::from(b)),
                    Some(Cell::Int(i)) => (MixedTag::Int, i as u64),
                    Some(Cell::Float(f)) => (MixedTag::Float, f.to_bits()),
                    Some(Cell::Text(t)) => {
                        arena.extend_from_slice(t.as_bytes());
                        (MixedTag::Text, 0)
                    }
                    Some(Cell::Bytes(b)) => {
                        arena.extend_from_slice(b);
                        (MixedTag::Bytes, 0)
                    }
                };
                tags.push(tag);
                numbers.push(number);
                let end = u32::try_from(arena.len()).expect("mixed arena exceeds 4 GiB");
                offsets.push(end);
            }
            (ColumnData::Bytes { offsets, arena }, c) => {
                if let Some(Cell::Bytes(b)) = c {
                    arena.extend_from_slice(b);
                }
                let end = u32::try_from(arena.len()).expect("byte arena exceeds 4 GiB");
                offsets.push(end);
            }
            _ => unreachable!("kind validated before push"),
        }
    }

    fn byte_size(&self) -> usize {
        match self {
            ColumnData::Bool(v) => v.capacity(),
            ColumnData::Int(v) => v.capacity() * 8,
            ColumnData::Float(v) => v.capacity() * 8,
            ColumnData::Text { offsets, arena } | ColumnData::Bytes { offsets, arena } => {
                offsets.capacity() * 4 + arena.capacity()
            }
            ColumnData::Mixed {
                tags,
                numbers,
                offsets,
                arena,
            } => {
                tags.capacity() + numbers.capacity() * 8 + offsets.capacity() * 4 + arena.capacity()
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Column {
    data: ColumnData,
    /// Bit `i` set means row `i` is non-NULL.
    validity: Vec<u64>,
}

impl Column {
    fn is_valid(&self, row: usize) -> bool {
        (self.validity[row / 64] >> (row % 64)) & 1 == 1
    }
}

/// Builds a [`ResultChunk`] row by row; a driver fills one per fetched page.
pub struct ChunkBuilder {
    columns: Arc<[ColumnMeta]>,
    data: Vec<Column>,
    len: usize,
}

impl ChunkBuilder {
    /// A builder for rows over `columns`.
    pub fn new(columns: Arc<[ColumnMeta]>) -> Self {
        let data = columns
            .iter()
            .map(|c| Column {
                data: ColumnData::new(c.kind),
                validity: Vec::new(),
            })
            .collect();
        Self {
            columns,
            data,
            len: 0,
        }
    }

    /// Appends one row. `Null` is valid for every kind. On error nothing is appended.
    pub fn push_row(&mut self, cells: &[Cell<'_>]) -> Result<(), ChunkError> {
        if cells.len() != self.columns.len() {
            return Err(ChunkError::Arity {
                expected: self.columns.len(),
                got: cells.len(),
            });
        }
        for (col, (cell, meta)) in cells.iter().zip(self.columns.iter()).enumerate() {
            let ok = matches!(
                (cell, meta.kind),
                (Cell::Null, _)
                    | (Cell::Bool(_), ColumnKind::Bool)
                    | (Cell::Int(_), ColumnKind::Int)
                    | (Cell::Float(_), ColumnKind::Float)
                    | (Cell::Text(_), ColumnKind::Text)
                    | (Cell::Bytes(_), ColumnKind::Bytes)
                    | (_, ColumnKind::Mixed)
            );
            if !ok {
                return Err(ChunkError::KindMismatch {
                    col,
                    kind: meta.kind,
                });
            }
        }
        let row = self.len;
        for (column, cell) in self.data.iter_mut().zip(cells) {
            if row.is_multiple_of(64) {
                column.validity.push(0);
            }
            match cell {
                Cell::Null => column.data.push(None),
                c => {
                    column.validity[row / 64] |= 1 << (row % 64);
                    column.data.push(Some(*c));
                }
            }
        }
        self.len += 1;
        Ok(())
    }

    /// Number of rows pushed so far.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no row was pushed.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Ends the builder and returns the immutable chunk.
    pub fn finish(self) -> ResultChunk {
        ResultChunk {
            columns: self.columns,
            len: self.len,
            data: Arc::new(ChunkData::Owned(self.data)),
            memory_owner: None,
        }
    }
}

/// An immutable, columnar block of result rows: the unit drivers stream and the engine stores. Cheap to clone; may be backed by spill storage.
#[derive(Clone, Debug)]
pub struct ResultChunk {
    columns: Arc<[ColumnMeta]>,
    len: usize,
    data: Arc<ChunkData>,
    // Accounting follows the payload through snapshots and background readers.
    memory_owner: Option<Arc<dyn fmt::Debug + Send + Sync>>,
}

#[derive(Debug)]
enum ChunkData {
    Owned(Vec<Column>),
    Mapped(spill::MappedChunk),
}

impl ResultChunk {
    /// The columns of every row.
    pub fn columns(&self) -> &Arc<[ColumnMeta]> {
        &self.columns
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the chunk has no row.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Panics if `row` or `col` is out of range.
    pub fn cell(&self, row: usize, col: usize) -> Cell<'_> {
        assert!(row < self.len, "row {row} out of range (len {})", self.len);
        let data = match self.data.as_ref() {
            ChunkData::Owned(data) => data,
            ChunkData::Mapped(data) => return data.cell(row, col),
        };
        let column = &data[col];
        if !column.is_valid(row) {
            return Cell::Null;
        }
        match &column.data {
            ColumnData::Bool(v) => Cell::Bool(v[row]),
            ColumnData::Int(v) => Cell::Int(v[row]),
            ColumnData::Float(v) => Cell::Float(v[row]),
            ColumnData::Text { offsets, arena } => {
                let bytes = &arena[offsets[row] as usize..offsets[row + 1] as usize];
                // The arena only ever receives whole `&str` values.
                Cell::Text(std::str::from_utf8(bytes).expect("arena holds valid UTF-8"))
            }
            ColumnData::Bytes { offsets, arena } => {
                Cell::Bytes(&arena[offsets[row] as usize..offsets[row + 1] as usize])
            }
            ColumnData::Mixed {
                tags,
                numbers,
                offsets,
                arena,
            } => {
                let bytes = &arena[offsets[row] as usize..offsets[row + 1] as usize];
                match tags[row] {
                    MixedTag::Null => Cell::Null,
                    MixedTag::Bool => Cell::Bool(numbers[row] != 0),
                    MixedTag::Int => Cell::Int(numbers[row] as i64),
                    MixedTag::Float => Cell::Float(f64::from_bits(numbers[row])),
                    // Text slices only ever come from whole `&str` values.
                    MixedTag::Text => {
                        Cell::Text(std::str::from_utf8(bytes).expect("arena holds valid UTF-8"))
                    }
                    MixedTag::Bytes => Cell::Bytes(bytes),
                }
            }
        }
    }

    /// Approximate payload size, retained after spilling for copy/export estimates.
    pub fn byte_size(&self) -> usize {
        let data = match self.data.as_ref() {
            ChunkData::Owned(data) => data,
            ChunkData::Mapped(data) => return data.payload_bytes,
        };
        data.iter()
            .map(|c| c.data.byte_size() + c.validity.capacity() * 8)
            .sum()
    }

    /// Heap bytes retained by the payload. File-backed pages are managed by the OS.
    pub fn resident_byte_size(&self) -> usize {
        if self.is_spilled() {
            0
        } else {
            self.byte_size()
        }
    }

    /// Whether the payload is read from spill storage rather than held in memory.
    pub fn is_spilled(&self) -> bool {
        matches!(self.data.as_ref(), ChunkData::Mapped(_))
    }

    /// Retains an allocation's accounting until its last snapshot releases the payload.
    pub fn with_memory_owner(mut self, owner: Arc<dyn fmt::Debug + Send + Sync>) -> Self {
        self.memory_owner = Some(owner);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn col(name: &str, kind: ColumnKind) -> ColumnMeta {
        ColumnMeta::new(name, name, kind, DisplayHint::Plain)
    }

    fn four() -> Arc<[ColumnMeta]> {
        Arc::from(vec![
            col("b", ColumnKind::Bool),
            col("i", ColumnKind::Int),
            col("f", ColumnKind::Float),
            col("t", ColumnKind::Text),
        ])
    }

    #[test]
    fn builds_and_reads_every_kind() {
        let mut b = ChunkBuilder::new(four());
        b.push_row(&[
            Cell::Bool(true),
            Cell::Int(-7),
            Cell::Float(1.5),
            Cell::Text("è🚀"),
        ])
        .unwrap();
        b.push_row(&[Cell::Null, Cell::Null, Cell::Null, Cell::Null])
            .unwrap();
        assert_eq!(b.len(), 2);
        let c = b.finish();
        assert_eq!(c.len(), 2);
        assert_eq!(c.cell(0, 0), Cell::Bool(true));
        assert_eq!(c.cell(0, 1), Cell::Int(-7));
        assert_eq!(c.cell(0, 2), Cell::Float(1.5));
        assert_eq!(c.cell(0, 3), Cell::Text("è🚀"));
        assert_eq!(c.cell(1, 1), Cell::Null);
        assert_eq!(c.cell(1, 3), Cell::Null);
    }

    #[test]
    fn arity_error() {
        let mut b = ChunkBuilder::new(four());
        let err = b
            .push_row(&[Cell::Null, Cell::Null, Cell::Null])
            .unwrap_err();
        assert_eq!(
            err,
            ChunkError::Arity {
                expected: 4,
                got: 3
            }
        );
    }

    #[test]
    fn kind_mismatch_error() {
        let mut b = ChunkBuilder::new(Arc::from(vec![col("i", ColumnKind::Int)]));
        let err = b.push_row(&[Cell::Text("x")]).unwrap_err();
        assert_eq!(
            err,
            ChunkError::KindMismatch {
                col: 0,
                kind: ColumnKind::Int
            }
        );
        assert_eq!(b.len(), 0);
    }

    #[test]
    fn display_cells() {
        assert_eq!(Cell::Null.to_string(), "NULL");
        assert_eq!(Cell::Bool(false).to_string(), "false");
        assert_eq!(Cell::Float(0.25).to_string(), "0.25");
    }

    #[test]
    fn bytes_column_round_trip() {
        let big = vec![0xab_u8; 1 << 20];
        let mut b = ChunkBuilder::new(Arc::from(vec![col("b", ColumnKind::Bytes)]));
        b.push_row(&[Cell::Bytes(&[1, 2])]).unwrap();
        b.push_row(&[Cell::Bytes(&[])]).unwrap();
        b.push_row(&[Cell::Null]).unwrap();
        b.push_row(&[Cell::Bytes(&big)]).unwrap();
        assert_eq!(
            b.push_row(&[Cell::Text("x")]).unwrap_err(),
            ChunkError::KindMismatch {
                col: 0,
                kind: ColumnKind::Bytes
            }
        );
        let c = b.finish();
        assert_eq!(c.len(), 4);
        assert_eq!(c.cell(0, 0), Cell::Bytes(&[1, 2]));
        assert_eq!(c.cell(1, 0), Cell::Bytes(&[]));
        assert_eq!(c.cell(2, 0), Cell::Null);
        assert_eq!(c.cell(3, 0), Cell::Bytes(&big));
        assert_eq!(Cell::Bytes(&[0, 0x1f]).to_string(), "0x001f");
    }

    #[test]
    fn mixed_column_keeps_each_storage_class() {
        let big = "t".repeat(1 << 20);
        let cells = [
            Cell::Int(-7),
            Cell::Float(1.5),
            Cell::Text("abc"),
            Cell::Null,
            Cell::Bytes(&[0, 0xff]),
            Cell::Bool(true),
            Cell::Text(""),
            Cell::Bytes(&[]),
            Cell::Text(&big),
            Cell::Int(i64::MIN),
            Cell::Float(f64::NEG_INFINITY),
        ];
        let mut b = ChunkBuilder::new(Arc::from(vec![
            col("m", ColumnKind::Mixed),
            col("i", ColumnKind::Int),
        ]));
        for (i, cell) in cells.iter().enumerate() {
            b.push_row(&[*cell, Cell::Int(i as i64)]).unwrap();
        }
        let c = b.finish();
        assert_eq!(c.len(), cells.len());
        for (i, cell) in cells.iter().enumerate() {
            assert_eq!(c.cell(i, 0), *cell, "row {i}");
            assert_eq!(c.cell(i, 1), Cell::Int(i as i64));
        }
        assert!(c.byte_size() > 1 << 20);
    }

    #[test]
    fn compare_cells_orders_like_sqlite() {
        use std::cmp::Ordering::*;
        let ordered = [
            Cell::Null,
            Cell::Int(-2),
            Cell::Float(-1.5),
            Cell::Bool(false),
            Cell::Int(1),
            Cell::Float(1.5),
            Cell::Int(2),
            Cell::Text(""),
            Cell::Text("B"),
            Cell::Text("a"),
            Cell::Bytes(&[]),
            Cell::Bytes(&[1]),
        ];
        for (i, a) in ordered.iter().enumerate() {
            for (j, b) in ordered.iter().enumerate() {
                assert_eq!(compare_cells(*a, *b), i.cmp(&j), "{a:?} vs {b:?}");
            }
        }
        assert_eq!(compare_cells(Cell::Int(1), Cell::Float(1.0)), Equal);
        assert_eq!(compare_cells(Cell::Float(f64::NAN), Cell::Int(0)), Less);
        assert_eq!(compare_cells(Cell::Int(i64::MAX), Cell::Float(1e30)), Less);
        assert_eq!(compare_cells(Cell::Text("x"), Cell::Int(9)), Greater);
    }

    #[test]
    fn column_meta_new_has_no_origin() {
        let m = ColumnMeta::new("a", "int4", ColumnKind::Int, DisplayHint::Number);
        assert_eq!(m.origin, None);
        let origin = ColumnOrigin {
            table: crate::table::TableRef {
                database: None,
                schema: Some("public".into()),
                name: "t".into(),
            },
            column: "a".into(),
        };
        let m = m.with_origin(Some(origin.clone()));
        assert_eq!(m.origin, Some(origin));
    }

    #[test]
    fn text_arena_holds_1mb_value() {
        let big = "x".repeat(1 << 20);
        let mut b = ChunkBuilder::new(Arc::from(vec![col("t", ColumnKind::Text)]));
        b.push_row(&[Cell::Text(&big)]).unwrap();
        let c = b.finish();
        match c.cell(0, 0) {
            Cell::Text(s) => assert_eq!(s.len(), 1 << 20),
            other => panic!("unexpected {other:?}"),
        }
    }
}
