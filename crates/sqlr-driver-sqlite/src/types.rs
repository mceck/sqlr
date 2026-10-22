//! Column hints from declared types, and cell decoding by storage class.

use std::sync::Arc;

use rusqlite::Statement;
use rusqlite::types::ValueRef;
use sqlr_core::{
    Cell, ChunkBuilder, ColumnKind, ColumnMeta, ColumnOrigin, DisplayHint, DriverError, ErrorKind,
    TableRef, Value,
};

/// How a declared type's values are shown. Every column is `ColumnKind::Mixed`, since
/// SQLite stores any value in any column; the declared type only picks the hint.
pub(crate) fn hint_from_decl(decl: Option<&str>) -> DisplayHint {
    let upper = decl.unwrap_or_default().to_ascii_uppercase();
    let has = |s: &str| upper.contains(s);
    if has("INT") || has("REAL") || has("FLOA") || has("DOUB") {
        DisplayHint::Number
    } else if has("BLOB") {
        DisplayHint::Binary
    } else if has("DEC") || has("NUMERIC") || has("MONEY") {
        DisplayHint::Decimal
    } else if has("DATE") || has("TIME") {
        DisplayHint::Temporal
    } else if has("JSON") {
        DisplayHint::Json
    } else if has("UUID") || has("GUID") {
        DisplayHint::Uuid
    } else {
        DisplayHint::Plain
    }
}

/// What the statement says about each result column before it runs.
pub(crate) struct Declared {
    pub(crate) name: String,
    pub(crate) decl: Option<String>,
    pub(crate) origin: Option<ColumnOrigin>,
}

pub(crate) fn declared(stmt: &Statement<'_>) -> Vec<Declared> {
    let decls: Vec<Option<String>> = stmt
        .columns()
        .iter()
        .map(|c| c.decl_type().map(String::from))
        .collect();
    stmt.columns_with_metadata()
        .iter()
        .zip(decls)
        .map(|(m, decl)| {
            let origin = match (m.database_name(), m.table_name(), m.origin_name()) {
                (Some(db), Some(table), Some(column)) => Some(ColumnOrigin {
                    table: TableRef {
                        database: None,
                        schema: Some(db.to_string()),
                        name: table.to_string(),
                    },
                    column: column.to_string(),
                }),
                _ => None,
            };
            Declared {
                name: m.name().to_string(),
                decl,
                origin,
            }
        })
        .collect()
}

pub(crate) fn column_metas(declared: Vec<Declared>) -> Arc<[ColumnMeta]> {
    declared
        .into_iter()
        .map(|d| {
            let hint = hint_from_decl(d.decl.as_deref());
            ColumnMeta::new(d.name, d.decl.unwrap_or_default(), ColumnKind::Mixed, hint)
                .with_origin(d.origin)
        })
        .collect()
}

pub(crate) fn to_sql(value: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match value {
        Value::Null => V::Null,
        Value::Bool(b) => V::Integer(i64::from(*b)),
        Value::Int(i) => V::Integer(*i),
        Value::Float(f) => V::Real(*f),
        Value::Text(t) => V::Text(t.clone()),
        Value::Bytes(b) => V::Blob(b.clone()),
    }
}

/// Appends one row, each cell as stored. Text that is not valid UTF-8 is shown lossily.
pub(crate) fn push_row(
    builder: &mut ChunkBuilder,
    values: &[ValueRef<'_>],
    scratch: &mut Vec<String>,
) -> Result<(), DriverError> {
    scratch.clear();
    for value in values {
        if let ValueRef::Text(t) = value
            && std::str::from_utf8(t).is_err()
        {
            scratch.push(String::from_utf8_lossy(t).into_owned());
        }
    }
    let mut lossy = scratch.iter();
    let cells: Vec<Cell<'_>> = values
        .iter()
        .map(|value| match *value {
            ValueRef::Null => Cell::Null,
            ValueRef::Integer(v) => Cell::Int(v),
            ValueRef::Real(v) => Cell::Float(v),
            ValueRef::Text(t) => match std::str::from_utf8(t) {
                Ok(text) => Cell::Text(text),
                Err(_) => Cell::Text(lossy.next().map_or("", String::as_str)),
            },
            ValueRef::Blob(b) => Cell::Bytes(b),
        })
        .collect();
    builder
        .push_row(&cells)
        .map_err(|e| DriverError::new(ErrorKind::Protocol, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_from_declared_types() {
        use DisplayHint as H;
        for (decl, expected) in [
            ("INTEGER", H::Number),
            ("bigint unsigned", H::Number),
            ("DOUBLE PRECISION", H::Number),
            ("VARCHAR(20)", H::Plain),
            ("BLOB", H::Binary),
            ("DECIMAL(10,2)", H::Decimal),
            ("NUMERIC", H::Decimal),
            ("DATETIME", H::Temporal),
            ("BOOLEAN", H::Plain),
            ("JSON", H::Json),
            ("", H::Plain),
        ] {
            assert_eq!(hint_from_decl(Some(decl)), expected, "{decl}");
        }
        assert_eq!(hint_from_decl(None), DisplayHint::Plain);
    }
}
