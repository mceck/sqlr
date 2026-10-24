//! All rows of one result, appended chunk by chunk (spec §7.2).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

use crate::chunk::{Cell, ColumnMeta, ResultChunk};

#[derive(Clone, Debug, Default)]
pub struct RowBuffer {
    columns: Arc<[ColumnMeta]>,
    chunks: Vec<Arc<ResultChunk>>,
    bytes: usize,
    revision: u64,
    /// `offsets[i]` is the global index of the first row of `chunks[i]`; the last entry is the total.
    offsets: Vec<usize>,
}

impl RowBuffer {
    pub fn new(columns: Arc<[ColumnMeta]>) -> Self {
        Self {
            columns,
            chunks: Vec::new(),
            bytes: 0,
            revision: 0,
            offsets: vec![0],
        }
    }

    pub fn columns(&self) -> &[ColumnMeta] {
        &self.columns
    }

    pub fn push_chunk(&mut self, chunk: ResultChunk) {
        debug_assert_eq!(
            **chunk.columns(),
            *self.columns,
            "chunk columns differ from buffer columns"
        );
        if self.offsets.is_empty() {
            self.offsets.push(0); // `Default` leaves the leading sentinel out
        }
        let total = self.row_count() + chunk.len();
        self.revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
        self.bytes += chunk.byte_size();
        self.chunks.push(Arc::new(chunk));
        self.offsets.push(total);
    }

    pub fn row_count(&self) -> usize {
        self.offsets.last().copied().unwrap_or(0)
    }

    pub fn cell(&self, row: usize, col: usize) -> Option<Cell<'_>> {
        if row >= self.row_count() || col >= self.columns.len() {
            return None;
        }
        // Last chunk starting at or before `row`. Empty chunks share their start with the
        // next chunk, so the last such start is the chunk that really holds `row`.
        let idx = self.offsets.partition_point(|&start| start <= row) - 1;
        Some(self.chunks[idx].cell(row - self.offsets[idx], col))
    }

    /// Identity of this immutable snapshot, changed when a chunk is appended.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn byte_size(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkBuilder, ColumnKind, DisplayHint};
    use std::sync::Arc;

    fn cols() -> Arc<[ColumnMeta]> {
        Arc::from(vec![ColumnMeta::new(
            "i",
            "int8",
            ColumnKind::Int,
            DisplayHint::Number,
        )])
    }

    fn chunk(cols: &Arc<[ColumnMeta]>, vals: &[i64]) -> ResultChunk {
        let mut b = ChunkBuilder::new(cols.clone());
        for v in vals {
            b.push_row(&[Cell::Int(*v)]).unwrap();
        }
        b.finish()
    }

    #[test]
    fn cells_across_chunk_boundaries() {
        let c = cols();
        let mut buf = RowBuffer::new(c.clone());
        buf.push_chunk(chunk(&c, &[0, 1, 2]));
        buf.push_chunk(chunk(&c, &[]));
        buf.push_chunk(chunk(&c, &[3, 4]));
        assert_eq!(buf.row_count(), 5);
        assert_eq!(buf.cell(2, 0), Some(Cell::Int(2)));
        assert_eq!(buf.cell(3, 0), Some(Cell::Int(3)));
        assert_eq!(buf.cell(4, 0), Some(Cell::Int(4)));
        assert_eq!(buf.cell(5, 0), None);
    }

    #[test]
    fn snapshots_share_chunk_payload_and_keep_their_rows() {
        let cols: Arc<[ColumnMeta]> = vec![ColumnMeta::new(
            "t",
            "text",
            ColumnKind::Text,
            DisplayHint::Plain,
        )]
        .into();
        let mut rows = RowBuffer::new(cols.clone());
        let mut b = ChunkBuilder::new(cols.clone());
        b.push_row(&[Cell::Text("shared payload")]).unwrap();
        rows.push_chunk(b.finish());
        let snapshot = rows.clone();
        let (Some(Cell::Text(a)), Some(Cell::Text(b))) = (rows.cell(0, 0), snapshot.cell(0, 0))
        else {
            panic!("text cells")
        };
        assert_eq!(
            a.as_ptr(),
            b.as_ptr(),
            "snapshots must not copy chunk payloads"
        );
        let mut b = ChunkBuilder::new(cols);
        b.push_row(&[Cell::Text("later")]).unwrap();
        rows.push_chunk(b.finish());
        assert_eq!(snapshot.row_count(), 1);
        assert_eq!(rows.row_count(), 2);
        assert!(rows.byte_size() > snapshot.byte_size());
    }

    #[test]
    fn empty_buffer() {
        let buf = RowBuffer::new(cols());
        assert_eq!(buf.row_count(), 0);
        assert_eq!(buf.cell(0, 0), None);
    }
}
