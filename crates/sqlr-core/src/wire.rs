//! Binary encodings of [`ResultChunk`] and [`DocChunk`] (spec §7.2): one format in process,
//! and over a sidecar's pipe. Local spill storage uses indexed column arrays for mapped reads.
//!
//! Numbers are little-endian; lengths and counts are LEB128 varints. Decoding is total: any
//! byte string either decodes or fails with a [`WireError`], and no length a buffer claims
//! is allocated before the buffer proves it holds that many bytes.
//!
//! A result chunk is `version(1) · columns · rows`, with each column's metadata first and
//! then, per column, a validity bitmap (bit set = not NULL), the byte length of its values
//! and the values of the non-NULL rows only.

use std::sync::Arc;

use crate::chunk::{
    Cell, ChunkBuilder, ColumnKind, ColumnMeta, ColumnOrigin, DisplayHint, ResultChunk,
};
use crate::docchunk::DocChunk;
use crate::table::TableRef;

const VERSION: u8 = 1;
/// Rows a chunk with no columns may claim (such a chunk holds no data to bound them).
const MAX_ROWS_WITHOUT_COLUMNS: usize = 1 << 16;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("the data ends before the chunk does")]
    Truncated,
    #[error("unknown chunk version {0}")]
    Version(u8),
    #[error("{0} unexpected bytes after the chunk")]
    Trailing(usize),
    #[error("invalid value tag {0}")]
    Tag(u8),
    #[error("text is not valid UTF-8")]
    Utf8,
    #[error("{0}")]
    Invalid(&'static str),
}

// ----- writing -----

fn put_varint(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn put_bytes(b: &[u8], out: &mut Vec<u8>) {
    put_varint(b.len() as u64, out);
    out.extend_from_slice(b);
}

fn put_opt_str(s: &Option<String>, out: &mut Vec<u8>) {
    match s {
        Some(s) => {
            out.push(1);
            put_bytes(s.as_bytes(), out);
        }
        None => out.push(0),
    }
}

fn kind_code(kind: ColumnKind) -> u8 {
    match kind {
        ColumnKind::Bool => 0,
        ColumnKind::Int => 1,
        ColumnKind::Float => 2,
        ColumnKind::Text => 3,
        ColumnKind::Bytes => 4,
        ColumnKind::Mixed => 5,
    }
}

fn kind_from(code: u8) -> Result<ColumnKind, WireError> {
    Ok(match code {
        0 => ColumnKind::Bool,
        1 => ColumnKind::Int,
        2 => ColumnKind::Float,
        3 => ColumnKind::Text,
        4 => ColumnKind::Bytes,
        5 => ColumnKind::Mixed,
        other => return Err(WireError::Tag(other)),
    })
}

const HINTS: [DisplayHint; 11] = [
    DisplayHint::Plain,
    DisplayHint::Number,
    DisplayHint::Decimal,
    DisplayHint::Temporal,
    DisplayHint::Interval,
    DisplayHint::Json,
    DisplayHint::Xml,
    DisplayHint::Uuid,
    DisplayHint::Array,
    DisplayHint::Geometry,
    DisplayHint::Binary,
];

fn hint_code(hint: DisplayHint) -> u8 {
    HINTS
        .iter()
        .position(|h| *h == hint)
        .expect("every hint is listed") as u8
}

/// Appends the encoding of `chunk` to `out`.
pub fn encode_chunk(chunk: &ResultChunk, out: &mut Vec<u8>) {
    out.push(VERSION);
    put_varint(chunk.columns().len() as u64, out);
    for c in chunk.columns().iter() {
        put_bytes(c.name.as_bytes(), out);
        put_bytes(c.type_name.as_bytes(), out);
        out.push(kind_code(c.kind));
        out.push(hint_code(c.hint));
        match &c.origin {
            Some(o) => {
                out.push(1);
                put_opt_str(&o.table.database, out);
                put_opt_str(&o.table.schema, out);
                put_bytes(o.table.name.as_bytes(), out);
                put_bytes(o.column.as_bytes(), out);
            }
            None => out.push(0),
        }
    }
    let rows = chunk.len();
    put_varint(rows as u64, out);
    let mut values = Vec::new();
    for (col, meta) in chunk.columns().iter().enumerate() {
        let mut validity = vec![0u8; rows.div_ceil(8)];
        values.clear();
        for row in 0..rows {
            let cell = chunk.cell(row, col);
            if cell == Cell::Null {
                continue;
            }
            validity[row / 8] |= 1 << (row % 8);
            match (meta.kind, cell) {
                (ColumnKind::Bool, Cell::Bool(b)) => values.push(u8::from(b)),
                (ColumnKind::Int, Cell::Int(i)) => values.extend_from_slice(&i.to_le_bytes()),
                (ColumnKind::Float, Cell::Float(f)) => values.extend_from_slice(&f.to_le_bytes()),
                (ColumnKind::Text, Cell::Text(t)) => put_bytes(t.as_bytes(), &mut values),
                (ColumnKind::Bytes, Cell::Bytes(b)) => put_bytes(b, &mut values),
                (ColumnKind::Mixed, cell) => put_mixed(cell, &mut values),
                _ => unreachable!("a chunk's cells match its column kinds"),
            }
        }
        out.extend_from_slice(&validity);
        put_varint(values.len() as u64, out);
        out.extend_from_slice(&values);
    }
}

fn put_mixed(cell: Cell<'_>, out: &mut Vec<u8>) {
    match cell {
        Cell::Null => out.push(0),
        Cell::Bool(b) => {
            out.push(1);
            out.push(u8::from(b));
        }
        Cell::Int(i) => {
            out.push(2);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Cell::Float(f) => {
            out.push(3);
            out.extend_from_slice(&f.to_le_bytes());
        }
        Cell::Text(t) => {
            out.push(4);
            put_bytes(t.as_bytes(), out);
        }
        Cell::Bytes(b) => {
            out.push(5);
            put_bytes(b, out);
        }
    }
}

// ----- reading -----

struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.at
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if n > self.remaining() {
            return Err(WireError::Truncated);
        }
        let s = &self.buf[self.at..self.at + n];
        self.at += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        Ok(self.take(N)?.try_into().expect("take returned N bytes"))
    }

    fn varint(&mut self) -> Result<u64, WireError> {
        let (mut n, mut shift) = (0u64, 0u32);
        loop {
            let b = self.u8()?;
            if shift >= 64 || (shift == 63 && b > 1) {
                return Err(WireError::Invalid("varint overflows 64 bits"));
            }
            n |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(n);
            }
            shift += 7;
        }
    }

    /// A length that must be backed by that many remaining bytes.
    fn len(&mut self) -> Result<usize, WireError> {
        let n = self.varint()?;
        if n > self.remaining() as u64 {
            return Err(WireError::Truncated);
        }
        Ok(n as usize)
    }

    fn bytes(&mut self) -> Result<&'a [u8], WireError> {
        let n = self.len()?;
        self.take(n)
    }

    fn str(&mut self) -> Result<&'a str, WireError> {
        std::str::from_utf8(self.bytes()?).map_err(|_| WireError::Utf8)
    }

    fn string(&mut self) -> Result<String, WireError> {
        self.str().map(str::to_string)
    }

    fn opt_string(&mut self) -> Result<Option<String>, WireError> {
        match self.u8()? {
            0 => Ok(None),
            1 => self.string().map(Some),
            other => Err(WireError::Tag(other)),
        }
    }
}

/// Decodes one chunk; the whole of `bytes` must be that chunk.
pub fn decode_chunk(bytes: &[u8]) -> Result<ResultChunk, WireError> {
    let mut r = Reader::new(bytes);
    let version = r.u8()?;
    if version != VERSION {
        return Err(WireError::Version(version));
    }
    // A column's metadata is at least 5 bytes, so the count is bounded by the buffer.
    let ncols = r.varint()?;
    if ncols > (r.remaining() / 5) as u64 {
        return Err(WireError::Truncated);
    }
    let mut metas = Vec::with_capacity(ncols as usize);
    for _ in 0..ncols {
        let name = r.string()?;
        let type_name = r.string()?;
        let kind = kind_from(r.u8()?)?;
        let hint_code = r.u8()?;
        let hint = *HINTS
            .get(usize::from(hint_code))
            .ok_or(WireError::Tag(hint_code))?;
        let origin = match r.u8()? {
            0 => None,
            1 => Some(ColumnOrigin {
                table: TableRef {
                    database: r.opt_string()?,
                    schema: r.opt_string()?,
                    name: r.string()?,
                },
                column: r.string()?,
            }),
            other => return Err(WireError::Tag(other)),
        };
        metas.push(ColumnMeta {
            name,
            type_name,
            kind,
            hint,
            origin,
        });
    }
    let rows = r.varint()?;
    if metas.is_empty() {
        if rows > MAX_ROWS_WITHOUT_COLUMNS as u64 {
            return Err(WireError::Invalid(
                "too many rows for a chunk with no columns",
            ));
        }
    } else if rows.div_ceil(8) > r.remaining() as u64 {
        // Every column carries one validity bit per row.
        return Err(WireError::Truncated);
    }
    let rows = rows as usize;

    struct Col<'a> {
        validity: &'a [u8],
        values: Reader<'a>,
    }
    let mut cols = Vec::with_capacity(metas.len());
    for _ in &metas {
        let validity = r.take(rows.div_ceil(8))?;
        let n = r.len()?;
        let values = Reader::new(r.take(n)?);
        cols.push(Col { validity, values });
    }
    if r.remaining() > 0 {
        return Err(WireError::Trailing(r.remaining()));
    }

    let columns: Arc<[ColumnMeta]> = Arc::from(metas);
    let mut builder = ChunkBuilder::new(columns.clone());
    let mut cells: Vec<Cell<'_>> = Vec::with_capacity(columns.len());
    for row in 0..rows {
        cells.clear();
        for (meta, col) in columns.iter().zip(cols.iter_mut()) {
            if col.validity[row / 8] >> (row % 8) & 1 == 0 {
                cells.push(Cell::Null);
                continue;
            }
            let v = &mut col.values;
            cells.push(match meta.kind {
                ColumnKind::Bool => Cell::Bool(match v.u8()? {
                    0 => false,
                    1 => true,
                    other => return Err(WireError::Tag(other)),
                }),
                ColumnKind::Int => Cell::Int(i64::from_le_bytes(v.array()?)),
                ColumnKind::Float => Cell::Float(f64::from_le_bytes(v.array()?)),
                ColumnKind::Text => Cell::Text(v.str()?),
                ColumnKind::Bytes => Cell::Bytes(v.bytes()?),
                ColumnKind::Mixed => match v.u8()? {
                    0 => Cell::Null,
                    1 => Cell::Bool(v.u8()? != 0),
                    2 => Cell::Int(i64::from_le_bytes(v.array()?)),
                    3 => Cell::Float(f64::from_le_bytes(v.array()?)),
                    4 => Cell::Text(v.str()?),
                    5 => Cell::Bytes(v.bytes()?),
                    other => return Err(WireError::Tag(other)),
                },
            });
        }
        builder
            .push_row(&cells)
            .map_err(|_| WireError::Invalid("a cell does not match its column"))?;
    }
    if cols.iter().any(|c| c.values.remaining() > 0) {
        return Err(WireError::Invalid("a column holds more values than rows"));
    }
    Ok(builder.finish())
}

// ----- documents -----

/// Appends the encoding of `chunk` to `out`: the document count, then each document's byte
/// length and bytes.
pub fn encode_doc_chunk(chunk: &DocChunk, out: &mut Vec<u8>) {
    out.push(VERSION);
    put_varint(chunk.len() as u64, out);
    for i in 0..chunk.len() {
        put_bytes(chunk.doc_bytes(i), out);
    }
}

/// Decodes one document chunk; every document is walked and checked (tags, lengths, UTF-8,
/// nesting depth) before the chunk is built, so reading it back cannot panic.
pub fn decode_doc_chunk(bytes: &[u8]) -> Result<DocChunk, WireError> {
    let mut r = Reader::new(bytes);
    let version = r.u8()?;
    if version != VERSION {
        return Err(WireError::Version(version));
    }
    let n = r.varint()?;
    // Every document is at least a length byte and a tag byte.
    if n > (r.remaining() / 2) as u64 {
        return Err(WireError::Truncated);
    }
    let mut chunk = DocChunk::default();
    for _ in 0..n {
        let doc = r.bytes()?;
        chunk.push_checked(doc).map_err(WireError::Invalid)?;
    }
    if r.remaining() > 0 {
        return Err(WireError::Trailing(r.remaining()));
    }
    Ok(chunk)
}
