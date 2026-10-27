//! A page of documents in one compact byte buffer (the document family's `ResultChunk`).
//!
//! Each value is a tag byte and its payload; lengths and counts are LEB128 varints, numbers
//! little-endian. A page of documents costs one allocation plus an offset per document,
//! instead of a tree of `String`s and `Vec`s.

use crate::doc::DocValue;

const NULL: u8 = 0;
const FALSE: u8 = 1;
const TRUE: u8 = 2;
const INT32: u8 = 3;
const INT64: u8 = 4;
const DOUBLE: u8 = 5;
const DECIMAL: u8 = 6;
const STRING: u8 = 7;
const OBJECT_ID: u8 = 8;
const DATE: u8 = 9;
const BINARY: u8 = 10;
const REGEX: u8 = 11;
const TIMESTAMP: u8 = 12;
const MIN_KEY: u8 = 13;
const MAX_KEY: u8 = 14;
const ARRAY: u8 = 15;
const DOCUMENT: u8 = 16;
const UNDEFINED: u8 = 17;

/// A document did not fit: a chunk holds at most 4 GiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocChunkFull;

impl std::fmt::Display for DocChunkFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a page of documents cannot exceed 4 GiB")
    }
}

impl std::error::Error for DocChunkFull {}

/// A Doc chunk: a page of documents in one contiguous encoded buffer, the document counterpart of a Result chunk.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocChunk {
    buf: Vec<u8>,
    /// Start of each document in `buf`.
    offsets: Vec<u32>,
}

impl DocChunk {
    /// A chunk holding `docs` in order.
    pub fn from_docs(docs: &[DocValue]) -> Self {
        let mut chunk = DocChunk::default();
        for d in docs {
            chunk
                .push(d)
                .expect("a page of documents fits in a chunk's 4 GiB");
        }
        chunk.buf.shrink_to_fit();
        chunk
    }

    /// Appends one document; fails, appending nothing, when the chunk would pass 4 GiB
    /// (its offsets are 32-bit).
    pub fn push(&mut self, doc: &DocValue) -> Result<(), DocChunkFull> {
        self.push_limited(doc, u32::MAX as usize)
    }

    fn push_limited(&mut self, doc: &DocValue, limit: usize) -> Result<(), DocChunkFull> {
        let start = self.buf.len();
        encode(doc, &mut self.buf);
        if self.buf.len() > limit {
            self.buf.truncate(start);
            return Err(DocChunkFull);
        }
        self.offsets.push(start as u32);
        Ok(())
    }

    /// The number of documents.
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// Whether the chunk holds no document.
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// The `i`-th document, decoded. Panics when `i >= len()`.
    pub fn doc(&self, i: usize) -> DocValue {
        let mut at = self.offsets[i] as usize;
        decode(&self.buf, &mut at)
    }

    /// The documents, decoded one by one.
    pub fn docs(&self) -> impl Iterator<Item = DocValue> + '_ {
        (0..self.len()).map(|i| self.doc(i))
    }

    /// The encoded bytes of the `i`-th document.
    pub(crate) fn doc_bytes(&self, i: usize) -> &[u8] {
        let start = self.offsets[i] as usize;
        let end = self
            .offsets
            .get(i + 1)
            .map_or(self.buf.len(), |&o| o as usize);
        &self.buf[start..end]
    }

    /// Appends a document already encoded, after walking it: every tag, length, string and
    /// nesting level is checked, and the document must fill `bytes` exactly.
    pub(crate) fn push_checked(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
        let mut at = 0;
        check(bytes, &mut at, 0)?;
        if at != bytes.len() {
            return Err("bytes after a document");
        }
        if self.buf.len() + bytes.len() > u32::MAX as usize {
            return Err("a page of documents cannot exceed 4 GiB");
        }
        self.offsets.push(self.buf.len() as u32);
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    /// Heap bytes held by the chunk.
    pub fn byte_size(&self) -> usize {
        self.buf.capacity() + self.offsets.capacity() * size_of::<u32>()
    }
}

fn put_len(n: usize, out: &mut Vec<u8>) {
    let mut n = n as u64;
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
    put_len(b.len(), out);
    out.extend_from_slice(b);
}

fn encode(v: &DocValue, out: &mut Vec<u8>) {
    match v {
        DocValue::Null => out.push(NULL),
        DocValue::Bool(b) => out.push(if *b { TRUE } else { FALSE }),
        DocValue::Int32(i) => {
            out.push(INT32);
            out.extend_from_slice(&i.to_le_bytes());
        }
        DocValue::Int64(i) => {
            out.push(INT64);
            out.extend_from_slice(&i.to_le_bytes());
        }
        DocValue::Double(d) => {
            out.push(DOUBLE);
            out.extend_from_slice(&d.to_le_bytes());
        }
        DocValue::Decimal128(s) => {
            out.push(DECIMAL);
            put_bytes(s.as_bytes(), out);
        }
        DocValue::String(s) => {
            out.push(STRING);
            put_bytes(s.as_bytes(), out);
        }
        DocValue::ObjectId(id) => {
            out.push(OBJECT_ID);
            out.extend_from_slice(id);
        }
        DocValue::DateTime(ms) => {
            out.push(DATE);
            out.extend_from_slice(&ms.to_le_bytes());
        }
        DocValue::Binary { subtype, bytes } => {
            out.push(BINARY);
            out.push(*subtype);
            put_bytes(bytes, out);
        }
        DocValue::Regex { pattern, options } => {
            out.push(REGEX);
            put_bytes(pattern.as_bytes(), out);
            put_bytes(options.as_bytes(), out);
        }
        DocValue::Timestamp { t, i } => {
            out.push(TIMESTAMP);
            out.extend_from_slice(&t.to_le_bytes());
            out.extend_from_slice(&i.to_le_bytes());
        }
        DocValue::MinKey => out.push(MIN_KEY),
        DocValue::MaxKey => out.push(MAX_KEY),
        DocValue::Undefined => out.push(UNDEFINED),
        DocValue::Array(items) => {
            out.push(ARRAY);
            put_len(items.len(), out);
            for item in items {
                encode(item, out);
            }
        }
        DocValue::Document(fields) => {
            out.push(DOCUMENT);
            put_len(fields.len(), out);
            for (k, item) in fields {
                put_bytes(k.as_bytes(), out);
                encode(item, out);
            }
        }
    }
}

/// Nesting a wire document may have; BSON itself stops at 100 levels.
const MAX_WIRE_DEPTH: usize = 128;

fn check_take<'a>(buf: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8], &'static str> {
    let end = at.checked_add(n).filter(|&e| e <= buf.len());
    let end = end.ok_or("a document ends early")?;
    let s = &buf[*at..end];
    *at = end;
    Ok(s)
}

fn check_len(buf: &[u8], at: &mut usize) -> Result<usize, &'static str> {
    let (mut n, mut shift) = (0u64, 0u32);
    loop {
        let b = check_take(buf, at, 1)?[0];
        if shift >= 64 || (shift == 63 && b > 1) {
            return Err("a length overflows 64 bits");
        }
        n |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            // A length is backed by that many bytes, or (counts) by at least one per item.
            return usize::try_from(n)
                .ok()
                .filter(|&n| n <= buf.len())
                .ok_or("a length passes the document");
        }
        shift += 7;
    }
}

fn check_str(buf: &[u8], at: &mut usize) -> Result<(), &'static str> {
    let n = check_len(buf, at)?;
    std::str::from_utf8(check_take(buf, at, n)?)
        .map(|_| ())
        .map_err(|_| "a string is not UTF-8")
}

fn check(buf: &[u8], at: &mut usize, depth: usize) -> Result<(), &'static str> {
    if depth > MAX_WIRE_DEPTH {
        return Err("documents nest too deeply");
    }
    let tag = check_take(buf, at, 1)?[0];
    match tag {
        NULL | FALSE | TRUE | MIN_KEY | MAX_KEY | UNDEFINED => {}
        INT32 => {
            check_take(buf, at, 4)?;
        }
        INT64 | DOUBLE | DATE | TIMESTAMP => {
            check_take(buf, at, 8)?;
        }
        DECIMAL | STRING => check_str(buf, at)?,
        OBJECT_ID => {
            check_take(buf, at, 12)?;
        }
        BINARY => {
            check_take(buf, at, 1)?;
            let n = check_len(buf, at)?;
            check_take(buf, at, n)?;
        }
        REGEX => {
            check_str(buf, at)?;
            check_str(buf, at)?;
        }
        ARRAY => {
            let n = check_len(buf, at)?;
            for _ in 0..n {
                check(buf, at, depth + 1)?;
            }
        }
        DOCUMENT => {
            let n = check_len(buf, at)?;
            for _ in 0..n {
                check_str(buf, at)?;
                check(buf, at, depth + 1)?;
            }
        }
        _ => return Err("unknown value tag"),
    }
    Ok(())
}

fn take<'a>(buf: &'a [u8], at: &mut usize, n: usize) -> &'a [u8] {
    let s = &buf[*at..*at + n];
    *at += n;
    s
}

fn take_len(buf: &[u8], at: &mut usize) -> usize {
    let (mut n, mut shift) = (0u64, 0);
    loop {
        let b = buf[*at];
        *at += 1;
        n |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return n as usize;
        }
        shift += 7;
    }
}

fn take_str(buf: &[u8], at: &mut usize) -> String {
    let n = take_len(buf, at);
    // Only `encode` writes the buffer, from valid `String`s.
    String::from_utf8(take(buf, at, n).to_vec()).expect("chunk strings are UTF-8")
}

fn take_array<const N: usize>(buf: &[u8], at: &mut usize) -> [u8; N] {
    take(buf, at, N)
        .try_into()
        .expect("take returns exactly N bytes")
}

fn decode(buf: &[u8], at: &mut usize) -> DocValue {
    let tag = buf[*at];
    *at += 1;
    match tag {
        NULL => DocValue::Null,
        FALSE => DocValue::Bool(false),
        TRUE => DocValue::Bool(true),
        INT32 => DocValue::Int32(i32::from_le_bytes(take_array(buf, at))),
        INT64 => DocValue::Int64(i64::from_le_bytes(take_array(buf, at))),
        DOUBLE => DocValue::Double(f64::from_le_bytes(take_array(buf, at))),
        DECIMAL => DocValue::Decimal128(take_str(buf, at)),
        STRING => DocValue::String(take_str(buf, at)),
        OBJECT_ID => DocValue::ObjectId(take_array(buf, at)),
        DATE => DocValue::DateTime(i64::from_le_bytes(take_array(buf, at))),
        BINARY => {
            let subtype = take(buf, at, 1)[0];
            let n = take_len(buf, at);
            DocValue::Binary {
                subtype,
                bytes: take(buf, at, n).to_vec(),
            }
        }
        REGEX => DocValue::Regex {
            pattern: take_str(buf, at),
            options: take_str(buf, at),
        },
        TIMESTAMP => DocValue::Timestamp {
            t: u32::from_le_bytes(take_array(buf, at)),
            i: u32::from_le_bytes(take_array(buf, at)),
        },
        MIN_KEY => DocValue::MinKey,
        MAX_KEY => DocValue::MaxKey,
        UNDEFINED => DocValue::Undefined,
        ARRAY => {
            let n = take_len(buf, at);
            DocValue::Array((0..n).map(|_| decode(buf, at)).collect())
        }
        DOCUMENT => {
            let n = take_len(buf, at);
            DocValue::Document(
                (0..n)
                    .map(|_| {
                        let k = take_str(buf, at);
                        (k, decode(buf, at))
                    })
                    .collect(),
            )
        }
        other => unreachable!("unknown chunk tag {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_chunk_refuses_documents_instead_of_wrapping_offsets() {
        let doc = DocValue::Document(vec![("a".into(), DocValue::String("x".repeat(10)))]);
        let mut chunk = DocChunk::default();
        assert!(chunk.push_limited(&doc, 20).is_ok());
        assert_eq!(
            chunk.push_limited(&doc, 20),
            Err(DocChunkFull),
            "over the limit"
        );
        assert_eq!(chunk.len(), 1, "nothing half-written");
        assert_eq!(chunk.doc(0), doc);
        assert!(chunk.push(&doc).is_ok());
        assert_eq!(chunk.len(), 2);
        assert_eq!(chunk.doc(1), doc);
    }
}
