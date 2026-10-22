//! Documents as rows and rows as documents (the "Table rule" of the document views and
//! of transfers between documents and tables).
//!
//! - Columns are the union of the top-level keys of the documents given (the first page), in
//!   first-seen order. A key a document lacks is `NULL`.
//! - Numbers, booleans and strings keep their type; ObjectIds show as their hex, dates as
//!   ISO 8601, decimals as their text, binary as bytes; nested documents and arrays (and the
//!   rarer BSON types) as compact relaxed Extended JSON text.
//! - A row becomes a flat document: one field per mapped column, values typed from the cell
//!   (text that is exactly an integer, a number or `true`/`false` reads as one, so `01234`
//!   stays text).

use std::collections::HashSet;
use std::sync::Arc;

use crate::chunk::{Cell, ChunkBuilder, ColumnKind, ColumnMeta, DisplayHint, ResultChunk};
use crate::doc::{DocValue, format_iso_date};
use crate::rows::RowBuffer;
use crate::value::Value;

/// The column type name shown in the header of every document column.
pub const DOC_COLUMN_TYPE: &str = "field";

/// The union of the top-level keys of `docs`, in first-seen order (a repeated key counts
/// once).
pub fn doc_columns<'a>(docs: impl IntoIterator<Item = &'a DocValue>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for doc in docs {
        if let DocValue::Document(fields) = doc {
            for (k, _) in fields {
                if seen.insert(k.as_str()) {
                    out.push(k.clone());
                }
            }
        }
    }
    out
}

/// One field as a cell value (see the module docs); `None` (a missing key) is `NULL`.
pub fn doc_field_value(v: Option<&DocValue>) -> Value {
    let Some(v) = v else {
        return Value::Null;
    };
    match v {
        DocValue::Null | DocValue::Undefined => Value::Null,
        DocValue::Bool(b) => Value::Bool(*b),
        DocValue::Int32(i) => Value::Int(i64::from(*i)),
        DocValue::Int64(i) => Value::Int(*i),
        DocValue::Double(d) => Value::Float(*d),
        DocValue::Decimal128(s) | DocValue::String(s) => Value::Text(s.clone()),
        DocValue::ObjectId(b) => Value::Text(crate::doc::hex(b)),
        DocValue::DateTime(ms) => Value::Text(format_iso_date(*ms)),
        DocValue::Binary { subtype: 0, bytes } => Value::Bytes(bytes.clone()),
        other => Value::Text(other.to_relaxed_json()),
    }
}

/// The column metas of document columns: mixed values, shown as text.
pub fn doc_column_metas(columns: &[String]) -> Arc<[ColumnMeta]> {
    columns
        .iter()
        .map(|name| ColumnMeta::new(name, DOC_COLUMN_TYPE, ColumnKind::Mixed, DisplayHint::Plain))
        .collect()
}

/// The values of `doc` under `columns` (a field repeated in the document: its first value).
pub fn doc_row(columns: &[String], doc: &DocValue) -> Vec<Value> {
    columns
        .iter()
        .map(|c| doc_field_value(doc.get(c)))
        .collect()
}

/// `docs` as one chunk with `metas` (from [`doc_column_metas`]).
pub fn doc_rows_chunk(metas: &Arc<[ColumnMeta]>, docs: &[DocValue]) -> ResultChunk {
    let columns: Vec<String> = metas.iter().map(|m| m.name.clone()).collect();
    let mut b = ChunkBuilder::new(metas.clone());
    for doc in docs {
        let row = doc_row(&columns, doc);
        let cells: Vec<Cell<'_>> = row.iter().map(Value::as_cell).collect();
        b.push_row(&cells)
            .expect("mixed columns take every cell kind");
    }
    b.finish()
}

/// The documents of `docs` as a table: columns from the first `first_page` documents.
pub fn docs_to_rows(docs: &[DocValue], first_page: usize) -> RowBuffer {
    let columns = doc_columns(docs.iter().take(first_page.max(1)));
    let metas = doc_column_metas(&columns);
    let mut rows = RowBuffer::new(metas.clone());
    if !docs.is_empty() && !columns.is_empty() {
        rows.push_chunk(doc_rows_chunk(&metas, docs));
    }
    rows
}

/// A cell as a document value; text is typed only when it reads back exactly (see the
/// module docs) and `typed_text` is set (delimited files carry text only).
pub fn value_to_doc(v: &Value, typed_text: bool) -> DocValue {
    match v {
        Value::Null => DocValue::Null,
        Value::Bool(b) => DocValue::Bool(*b),
        Value::Int(i) => int_doc(*i),
        Value::Float(f) => DocValue::Double(*f),
        Value::Bytes(b) => DocValue::Binary {
            subtype: 0,
            bytes: b.clone(),
        },
        Value::Text(t) if typed_text => typed(t),
        Value::Text(t) => DocValue::String(t.clone()),
    }
}

fn int_doc(i: i64) -> DocValue {
    match i32::try_from(i) {
        Ok(v) => DocValue::Int32(v),
        Err(_) => DocValue::Int64(i),
    }
}

fn typed(t: &str) -> DocValue {
    match t {
        "true" => return DocValue::Bool(true),
        "false" => return DocValue::Bool(false),
        _ => {}
    }
    if let Ok(i) = t.parse::<i64>()
        && i.to_string() == t
    {
        return int_doc(i);
    }
    if let Ok(f) = t.parse::<f64>()
        && f.is_finite()
        && t.contains('.')
        && !t.starts_with(['.', '+'])
        && !t.ends_with('.')
        && f.to_string() == t
    {
        return DocValue::Double(f);
    }
    DocValue::String(t.to_string())
}

/// A row as a flat document: `fields` pairs a row index with the field name it fills.
pub fn row_to_doc(fields: &[(usize, String)], row: &[Value], typed_text: bool) -> DocValue {
    DocValue::Document(
        fields
            .iter()
            .map(|(i, name)| {
                let v = row.get(*i).unwrap_or(&Value::Null);
                (name.clone(), value_to_doc(v, typed_text))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(fields: Vec<(&str, DocValue)>) -> DocValue {
        DocValue::Document(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    #[test]
    fn columns_are_the_union_of_keys_in_first_seen_order() {
        let docs = [
            doc(vec![("_id", DocValue::Int32(1)), ("b", DocValue::Null)]),
            doc(vec![("c", DocValue::Int32(2)), ("_id", DocValue::Int32(2))]),
            doc(vec![("a", DocValue::Int32(3))]),
        ];
        assert_eq!(doc_columns(&docs), ["_id", "b", "c", "a"]);
        let rows = docs_to_rows(&docs, 2);
        let names: Vec<&str> = rows.columns().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["_id", "b", "c"],
            "only the first page names columns"
        );
        assert_eq!(rows.row_count(), 3);
        assert_eq!(rows.cell(1, 1), Some(Cell::Null), "a missing key is NULL");
    }

    #[test]
    fn field_values_keep_scalars_and_write_nested_values_as_json() {
        assert_eq!(doc_field_value(Some(&DocValue::Int64(7))), Value::Int(7));
        assert_eq!(
            doc_field_value(Some(&DocValue::ObjectId([0xab; 12]))),
            Value::Text("ab".repeat(12))
        );
        assert_eq!(
            doc_field_value(Some(&DocValue::DateTime(0))),
            Value::Text("1970-01-01T00:00:00Z".into())
        );
        let nested = doc(vec![("city", DocValue::String("x".into()))]);
        assert_eq!(
            doc_field_value(Some(&nested)),
            Value::Text("{\"city\": \"x\"}".into())
        );
        assert_eq!(
            doc_field_value(Some(&DocValue::Array(vec![DocValue::Int32(1)]))),
            Value::Text("[1]".into())
        );
    }

    #[test]
    fn rows_become_flat_typed_documents() {
        let row = [
            Value::Text("alice".into()),
            Value::Text("30".into()),
            Value::Text("1.5".into()),
            Value::Text("01234".into()),
            Value::Text("true".into()),
            Value::Null,
            Value::Text("1e3".into()),
        ];
        let fields: Vec<(usize, String)> = ["name", "age", "score", "zip", "ok", "none", "e"]
            .iter()
            .enumerate()
            .map(|(i, n)| (i, n.to_string()))
            .collect();
        assert_eq!(
            row_to_doc(&fields, &row, true),
            doc(vec![
                ("name", DocValue::String("alice".into())),
                ("age", DocValue::Int32(30)),
                ("score", DocValue::Double(1.5)),
                ("zip", DocValue::String("01234".into())),
                ("ok", DocValue::Bool(true)),
                ("none", DocValue::Null),
                ("e", DocValue::String("1e3".into())),
            ])
        );
        assert_eq!(
            row_to_doc(&fields[..2], &row, false),
            doc(vec![
                ("name", DocValue::String("alice".into())),
                ("age", DocValue::String("30".into())),
            ])
        );
        assert_eq!(
            value_to_doc(&Value::Int(1 << 40), false),
            DocValue::Int64(1 << 40)
        );
    }
}
