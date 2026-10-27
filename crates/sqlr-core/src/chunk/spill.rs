//! Pure columnar encoding and zero-copy reading of immutable spill storage.

use super::{Cell, ChunkData, ColumnData, MixedTag, ResultChunk};
use std::sync::Arc;

/// Immutable storage supplied by the engine. The core performs no file I/O.
pub trait SpillBacking: AsRef<[u8]> + std::fmt::Debug + Send + Sync {}
impl<T: AsRef<[u8]> + std::fmt::Debug + Send + Sync> SpillBacking for T {}

/// One chunk's payload and small per-column index, ready for background storage.
pub struct SpillEncoding {
    bytes: Vec<u8>,
    columns: Vec<MappedColumn>,
    metadata: Arc<[super::ColumnMeta]>,
    rows: usize,
    payload_bytes: usize,
}

impl SpillEncoding {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Validates the backing against the encoded payload before publishing it.
    pub fn into_chunk(self, backing: Arc<dyn SpillBacking>) -> Result<ResultChunk, &'static str> {
        if backing.as_ref().as_ref() != self.bytes {
            return Err("spill storage differs from the encoded chunk");
        }
        Ok(ResultChunk {
            columns: self.metadata,
            len: self.rows,
            data: Arc::new(ChunkData::Mapped(MappedChunk {
                map: backing,
                columns: self.columns,
                payload_bytes: self.payload_bytes,
            })),
            memory_owner: None,
        })
    }
}

impl ResultChunk {
    /// Encodes a single heap-backed chunk for spilling. File work belongs to the caller.
    pub fn encode_spill(&self) -> Option<SpillEncoding> {
        let ChunkData::Owned(columns) = self.data.as_ref() else {
            return None;
        };
        if self.byte_size() == 0 {
            return None;
        }
        let mut writer = Writer {
            bytes: Vec::new(),
            offset: 0,
        };
        let mut mapped_columns = Vec::with_capacity(columns.len());
        for column in columns {
            let validity = writer.offset;
            for bits in &column.validity {
                writer.bytes(&bits.to_le_bytes());
            }
            let data = match &column.data {
                ColumnData::Bool(values) => {
                    let start = writer.offset;
                    for value in values {
                        writer.bytes(&[u8::from(*value)]);
                    }
                    MappedData::Bool(start)
                }
                ColumnData::Int(values) => {
                    let start = writer.offset;
                    for value in values {
                        writer.bytes(&value.to_le_bytes());
                    }
                    MappedData::Int(start)
                }
                ColumnData::Float(values) => {
                    let start = writer.offset;
                    for value in values {
                        writer.bytes(&value.to_le_bytes());
                    }
                    MappedData::Float(start)
                }
                ColumnData::Text { offsets, arena } | ColumnData::Bytes { offsets, arena } => {
                    let offsets_start = writer.offset;
                    for offset in offsets {
                        writer.bytes(&offset.to_le_bytes());
                    }
                    let arena_start = writer.bytes(arena);
                    if matches!(column.data, ColumnData::Text { .. }) {
                        MappedData::Text {
                            offsets: offsets_start,
                            arena: arena_start,
                        }
                    } else {
                        MappedData::Bytes {
                            offsets: offsets_start,
                            arena: arena_start,
                        }
                    }
                }
                ColumnData::Mixed {
                    tags,
                    numbers,
                    offsets,
                    arena,
                } => {
                    let tags_start = writer.offset;
                    for tag in tags {
                        writer.bytes(&[*tag as u8]);
                    }
                    let numbers_start = writer.offset;
                    for number in numbers {
                        writer.bytes(&number.to_le_bytes());
                    }
                    let offsets_start = writer.offset;
                    for offset in offsets {
                        writer.bytes(&offset.to_le_bytes());
                    }
                    let arena_start = writer.bytes(arena);
                    MappedData::Mixed {
                        tags: tags_start,
                        numbers: numbers_start,
                        offsets: offsets_start,
                        arena: arena_start,
                    }
                }
            };
            mapped_columns.push(MappedColumn { validity, data });
        }

        Some(SpillEncoding {
            bytes: writer.bytes,
            columns: mapped_columns,
            metadata: self.columns.clone(),
            rows: self.len,
            payload_bytes: self.byte_size(),
        })
    }
}

struct Writer {
    bytes: Vec<u8>,
    offset: usize,
}
impl Writer {
    fn bytes(&mut self, bytes: &[u8]) -> usize {
        let start = self.offset;
        self.bytes.extend_from_slice(bytes);
        self.offset = self.bytes.len();
        start
    }
}

pub(super) struct MappedChunk {
    map: Arc<dyn SpillBacking>,
    columns: Vec<MappedColumn>,
    pub(super) payload_bytes: usize,
}

impl std::fmt::Debug for MappedChunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedChunk")
            .field("bytes", &self.map.as_ref().as_ref().len())
            .field("columns", &self.columns.len())
            .finish()
    }
}

struct MappedColumn {
    validity: usize,
    data: MappedData,
}

enum MappedData {
    Bool(usize),
    Int(usize),
    Float(usize),
    Text {
        offsets: usize,
        arena: usize,
    },
    Bytes {
        offsets: usize,
        arena: usize,
    },
    Mixed {
        tags: usize,
        numbers: usize,
        offsets: usize,
        arena: usize,
    },
}

impl MappedChunk {
    pub(super) fn cell(&self, row: usize, col: usize) -> Cell<'_> {
        let column = &self.columns[col];
        if self.map.as_ref().as_ref()[column.validity + row / 8] & (1 << (row % 8)) == 0 {
            return Cell::Null;
        }
        match column.data {
            MappedData::Bool(start) => Cell::Bool(self.map.as_ref().as_ref()[start + row] != 0),
            MappedData::Int(start) => Cell::Int(self.u64(start + row * 8) as i64),
            MappedData::Float(start) => Cell::Float(f64::from_bits(self.u64(start + row * 8))),
            MappedData::Text { offsets, arena } => {
                Cell::Text(self.text(self.bytes(row, offsets, arena)))
            }
            MappedData::Bytes { offsets, arena } => Cell::Bytes(self.bytes(row, offsets, arena)),
            MappedData::Mixed {
                tags,
                numbers,
                offsets,
                arena,
            } => {
                let tag = self.map.as_ref().as_ref()[tags + row];
                let number = self.u64(numbers + row * 8);
                match tag {
                    t if t == MixedTag::Bool as u8 => Cell::Bool(number != 0),
                    t if t == MixedTag::Int as u8 => Cell::Int(number as i64),
                    t if t == MixedTag::Float as u8 => Cell::Float(f64::from_bits(number)),
                    t if t == MixedTag::Text as u8 => {
                        Cell::Text(self.text(self.bytes(row, offsets, arena)))
                    }
                    t if t == MixedTag::Bytes as u8 => Cell::Bytes(self.bytes(row, offsets, arena)),
                    _ => Cell::Null,
                }
            }
        }
    }

    fn u64(&self, start: usize) -> u64 {
        u64::from_le_bytes(std::array::from_fn(|i| {
            self.map.as_ref().as_ref()[start + i]
        }))
    }

    fn bytes(&self, row: usize, offsets: usize, arena: usize) -> &[u8] {
        let offset = |index| {
            u32::from_le_bytes(std::array::from_fn(|i| {
                self.map.as_ref().as_ref()[offsets + index * 4 + i]
            })) as usize
        };
        &self.map.as_ref().as_ref()[arena + offset(row)..arena + offset(row + 1)]
    }

    fn text<'a>(&self, bytes: &'a [u8]) -> &'a str {
        std::str::from_utf8(bytes).expect("spill backing preserves validated UTF-8 arenas")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChunkBuilder, ColumnKind, ColumnMeta, DisplayHint};

    #[test]
    fn spill_encoding_preserves_all_cell_kinds_null_bitmaps_and_wire_values() {
        let columns: Arc<[_]> = [
            ColumnKind::Bool,
            ColumnKind::Int,
            ColumnKind::Float,
            ColumnKind::Text,
            ColumnKind::Bytes,
            ColumnKind::Mixed,
        ]
        .into_iter()
        .map(|kind| ColumnMeta::new(format!("{kind:?}"), "native", kind, DisplayHint::Plain))
        .collect();
        let mut builder = ChunkBuilder::new(columns);
        for row in 0..131 {
            let mixed = match row % 6 {
                0 => Cell::Null,
                1 => Cell::Bool(true),
                2 => Cell::Int(i64::MIN),
                3 => Cell::Float(-1.25),
                4 => Cell::Text("混合😀"),
                _ => Cell::Bytes(&[0, 255]),
            };
            let cells = if row % 11 == 0 {
                [Cell::Null; 6]
            } else {
                [
                    Cell::Bool(row % 2 == 0),
                    Cell::Int(-row),
                    Cell::Float(row as f64 / 4.0),
                    Cell::Text(if row % 3 == 0 { "" } else { "hello 東京😀" }),
                    Cell::Bytes(&[0, 128, 255]),
                    mixed,
                ]
            };
            builder.push_row(&cells).unwrap();
        }
        let original = builder.finish();
        let encoded = original.encode_spill().unwrap();
        let bytes = encoded.bytes().to_vec();
        let mapped = encoded.into_chunk(Arc::new(bytes)).unwrap();
        assert!(mapped.is_spilled());
        assert_eq!(mapped.resident_byte_size(), 0);
        assert_eq!(mapped.byte_size(), original.byte_size());
        for row in 0..131 {
            for col in 0..6 {
                assert_eq!(original.cell(row, col), mapped.cell(row, col));
            }
        }
        let mut before = Vec::new();
        let mut after = Vec::new();
        crate::wire::encode_chunk(&original, &mut before);
        crate::wire::encode_chunk(&mapped, &mut after);
        assert_eq!(before, after);
    }

    #[test]
    fn spill_encoding_rejects_incomplete_or_changed_storage() {
        let columns = vec![ColumnMeta::new(
            "n",
            "int",
            ColumnKind::Int,
            DisplayHint::Number,
        )]
        .into();
        let mut builder = ChunkBuilder::new(columns);
        builder.push_row(&[Cell::Int(42)]).unwrap();
        let chunk = builder.finish();
        let encoded = chunk.encode_spill().unwrap();
        let mut bytes = encoded.bytes().to_vec();
        bytes.pop();
        assert!(encoded.into_chunk(Arc::new(bytes)).is_err());
        assert_eq!(chunk.cell(0, 0), Cell::Int(42));
    }
}
