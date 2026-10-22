//! The binary encodings of result chunks and document chunks (spec §7.2).

use std::sync::Arc;

use sqlr_core::{
    Cell, ChunkBuilder, ColumnKind, ColumnMeta, ColumnOrigin, DisplayHint, DocChunk, DocValue,
    ResultChunk, TableRef, WireError, decode_chunk, decode_doc_chunk, encode_chunk,
    encode_doc_chunk,
};

fn meta(name: &str, kind: ColumnKind, hint: DisplayHint) -> ColumnMeta {
    ColumnMeta::new(name, format!("{name}_type"), kind, hint)
}

fn all_kinds() -> Arc<[ColumnMeta]> {
    Arc::from(vec![
        meta("b", ColumnKind::Bool, DisplayHint::Plain),
        meta("i", ColumnKind::Int, DisplayHint::Number),
        meta("f", ColumnKind::Float, DisplayHint::Number),
        meta("t", ColumnKind::Text, DisplayHint::Json),
        meta("y", ColumnKind::Bytes, DisplayHint::Binary),
        meta("m", ColumnKind::Mixed, DisplayHint::Plain).with_origin(Some(ColumnOrigin {
            table: TableRef {
                database: Some("db".into()),
                schema: None,
                name: "t".into(),
            },
            column: "m".into(),
        })),
    ])
}

fn same(a: &ResultChunk, b: &ResultChunk) {
    assert_eq!(a.columns(), b.columns());
    assert_eq!(a.len(), b.len());
    for row in 0..a.len() {
        for col in 0..a.columns().len() {
            let (x, y) = (a.cell(row, col), b.cell(row, col));
            match (x, y) {
                // NaN is the one value that is not equal to itself.
                (Cell::Float(p), Cell::Float(q)) if p.is_nan() => assert!(q.is_nan()),
                _ => assert_eq!(x, y, "row {row} col {col}"),
            }
        }
    }
}

fn round_trip(chunk: &ResultChunk) -> ResultChunk {
    let mut bytes = Vec::new();
    encode_chunk(chunk, &mut bytes);
    decode_chunk(&bytes).expect("decodes")
}

#[test]
fn wire_round_trip_every_kind() {
    let big = "ü".repeat(500_000); // 1 MB of text
    let mut b = ChunkBuilder::new(all_kinds());
    b.push_row(&[
        Cell::Bool(true),
        Cell::Int(i64::MIN),
        Cell::Float(f64::NAN),
        Cell::Text(&big),
        Cell::Bytes(&[0, 255, 7]),
        Cell::Text("mixed"),
    ])
    .unwrap();
    b.push_row(&[
        Cell::Null,
        Cell::Null,
        Cell::Null,
        Cell::Null,
        Cell::Null,
        Cell::Null,
    ])
    .unwrap();
    b.push_row(&[
        Cell::Bool(false),
        Cell::Int(i64::MAX),
        Cell::Float(-0.0),
        Cell::Text(""),
        Cell::Bytes(&[]),
        Cell::Int(42),
    ])
    .unwrap();
    for i in 0..130 {
        // More than 64 rows, so validity bitmaps span several words.
        b.push_row(&[
            Cell::Bool(i % 2 == 0),
            Cell::Int(i),
            Cell::Float(i as f64 / 3.0),
            Cell::Text("x"),
            Cell::Bytes(&[i as u8]),
            if i % 3 == 0 {
                Cell::Bytes(&[1, 2])
            } else {
                Cell::Float(1.5)
            },
        ])
        .unwrap();
    }
    let chunk = b.finish();
    same(&chunk, &round_trip(&chunk));

    let empty = ChunkBuilder::new(all_kinds()).finish();
    same(&empty, &round_trip(&empty));
    let no_columns = ChunkBuilder::new(Arc::from(Vec::<ColumnMeta>::new())).finish();
    same(&no_columns, &round_trip(&no_columns));
}

#[test]
fn decoder_rejects_malformed_frames() {
    let mut b = ChunkBuilder::new(all_kinds());
    b.push_row(&[
        Cell::Bool(true),
        Cell::Int(1),
        Cell::Float(1.0),
        Cell::Text("héllo"),
        Cell::Bytes(&[1, 2, 3]),
        Cell::Text("m"),
    ])
    .unwrap();
    let mut good = Vec::new();
    encode_chunk(&b.finish(), &mut good);
    assert!(decode_chunk(&good).is_ok());

    // Every truncation fails cleanly.
    for len in 0..good.len() {
        assert!(
            decode_chunk(&good[..len]).is_err(),
            "a {len}-byte prefix of {} decoded",
            good.len()
        );
    }
    // Trailing bytes are refused.
    let mut trailing = good.clone();
    trailing.push(0);
    assert_eq!(decode_chunk(&trailing).err(), Some(WireError::Trailing(1)));
    // Unknown version.
    let mut version = good.clone();
    version[0] = 200;
    assert!(matches!(
        decode_chunk(&version),
        Err(WireError::Version(200))
    ));
    // Flipping any single byte never panics (it may still decode to something valid).
    for i in 0..good.len() {
        let mut flipped = good.clone();
        flipped[i] ^= 0xff;
        let _ = decode_chunk(&flipped);
    }
    // A claimed length far past the buffer is refused without allocating it: 2^40 columns,
    // then a 2^40-byte string.
    let mut huge = vec![1u8];
    huge.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x20]);
    assert!(decode_chunk(&huge).is_err());
    let mut huge_str = vec![1u8, 1];
    huge_str.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x20]);
    assert!(decode_chunk(&huge_str).is_err());
    // Invalid UTF-8 in a column name.
    let bad_utf8 = [1u8, 1, 2, 0xff, 0xfe];
    assert!(matches!(
        decode_chunk(&bad_utf8),
        Err(WireError::Utf8 | WireError::Truncated)
    ));
    // Rows claimed with no data behind them.
    let mut rows = vec![1u8, 1, 1, b'a', 1, b't', 1, 0, 0];
    rows.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x0f]);
    assert!(decode_chunk(&rows).is_err());
}

#[test]
fn doc_chunk_round_trip_and_rejection() {
    let docs = vec![
        DocValue::Document(vec![
            ("_id".into(), DocValue::ObjectId([7; 12])),
            ("n".into(), DocValue::Int32(-1)),
            ("big".into(), DocValue::Int64(1 << 40)),
            ("d".into(), DocValue::Double(2.5)),
            ("dec".into(), DocValue::Decimal128("1.10".into())),
            ("s".into(), DocValue::String("héllo".into())),
            ("when".into(), DocValue::DateTime(1_700_000_000_000)),
            (
                "bin".into(),
                DocValue::Binary {
                    subtype: 4,
                    bytes: vec![1, 2, 3],
                },
            ),
            (
                "re".into(),
                DocValue::Regex {
                    pattern: "^a".into(),
                    options: "i".into(),
                },
            ),
            ("ts".into(), DocValue::Timestamp { t: 1, i: 2 }),
            (
                "arr".into(),
                DocValue::Array(vec![DocValue::Null, DocValue::Bool(true), DocValue::MinKey]),
            ),
            ("u".into(), DocValue::Undefined),
            ("max".into(), DocValue::MaxKey),
        ]),
        DocValue::Document(vec![]),
    ];
    let chunk = DocChunk::from_docs(&docs);
    let mut bytes = Vec::new();
    encode_doc_chunk(&chunk, &mut bytes);
    let back = decode_doc_chunk(&bytes).expect("decodes");
    assert_eq!(back.len(), 2);
    assert_eq!(back.docs().collect::<Vec<_>>(), docs);

    for len in 0..bytes.len() {
        assert!(decode_doc_chunk(&bytes[..len]).is_err(), "prefix {len}");
    }
    for i in 0..bytes.len() {
        let mut flipped = bytes.clone();
        flipped[i] ^= 0xff;
        let _ = decode_doc_chunk(&flipped); // never panics
    }
    // Nesting deeper than the limit is refused rather than recursing without bound.
    let mut deep = DocValue::Null;
    for _ in 0..1_000 {
        deep = DocValue::Array(vec![deep]);
    }
    let deep_chunk = {
        let mut c = DocChunk::default();
        c.push(&deep).unwrap();
        c
    };
    let mut deep_bytes = Vec::new();
    encode_doc_chunk(&deep_chunk, &mut deep_bytes);
    assert!(decode_doc_chunk(&deep_bytes).is_err());
}

/// A stand-in for the cargo-fuzz targets on machines without a nightly toolchain: many
/// seeded random mutations of valid encodings, none of which may panic.
#[test]
fn random_mutations_never_panic() {
    let mut b = ChunkBuilder::new(all_kinds());
    for i in 0..40 {
        b.push_row(&[
            Cell::Bool(i % 2 == 0),
            Cell::Int(i),
            Cell::Float(i as f64),
            Cell::Text("héllo"),
            Cell::Bytes(&[i as u8, 1]),
            if i % 2 == 0 {
                Cell::Text("m")
            } else {
                Cell::Int(i)
            },
        ])
        .unwrap();
    }
    let mut chunk_bytes = Vec::new();
    encode_chunk(&b.finish(), &mut chunk_bytes);
    let doc = DocValue::Document(vec![
        (
            "a".into(),
            DocValue::Array(vec![DocValue::Int32(1), DocValue::String("x".into())]),
        ),
        (
            "b".into(),
            DocValue::Binary {
                subtype: 0,
                bytes: vec![1, 2, 3],
            },
        ),
    ]);
    let mut doc_bytes = Vec::new();
    encode_doc_chunk(&DocChunk::from_docs(&[doc.clone(), doc]), &mut doc_bytes);

    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..20_000 {
        let mut bytes = if round % 2 == 0 {
            chunk_bytes.clone()
        } else {
            doc_bytes.clone()
        };
        for _ in 0..1 + next() % 4 {
            let at = (next() % bytes.len() as u64) as usize;
            match next() % 3 {
                0 => bytes[at] = next() as u8,
                1 => bytes[at] ^= 1 << (next() % 8),
                _ => bytes.truncate(at),
            }
            if bytes.is_empty() {
                break;
            }
        }
        let _ = decode_chunk(&bytes);
        if let Ok(chunk) = decode_doc_chunk(&bytes) {
            // Whatever decodes can be read back without panicking.
            let _ = chunk.docs().count();
        }
    }
}
