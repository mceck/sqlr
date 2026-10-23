//! Oracle values: decoded rows to sqlr cells, and bound parameters the other way.

use oracle_rs::{ColumnInfo, LobValue, OracleType, Value as OracleValue};
use sqlr_core::{ColumnKind, ColumnMeta, DisplayHint, Value};

/// The column an Oracle result describes.
pub(crate) fn column_meta(column: &ColumnInfo) -> ColumnMeta {
    let (type_name, kind, hint) = match column.oracle_type {
        OracleType::Varchar => (
            format!("VARCHAR2({})", column.data_size),
            ColumnKind::Text,
            DisplayHint::Plain,
        ),
        OracleType::Char => (
            format!("CHAR({})", column.data_size),
            ColumnKind::Text,
            DisplayHint::Plain,
        ),
        OracleType::Long | OracleType::Clob => {
            ("CLOB".to_string(), ColumnKind::Text, DisplayHint::Plain)
        }
        OracleType::Number => (
            number_type(column.precision, column.scale),
            ColumnKind::Mixed,
            DisplayHint::Decimal,
        ),
        OracleType::BinaryInteger => (
            "BINARY_INTEGER".to_string(),
            ColumnKind::Int,
            DisplayHint::Number,
        ),
        OracleType::BinaryFloat => (
            "BINARY_FLOAT".to_string(),
            ColumnKind::Float,
            DisplayHint::Number,
        ),
        OracleType::BinaryDouble => (
            "BINARY_DOUBLE".to_string(),
            ColumnKind::Float,
            DisplayHint::Number,
        ),
        OracleType::Date => ("DATE".to_string(), ColumnKind::Text, DisplayHint::Temporal),
        OracleType::Timestamp => (
            "TIMESTAMP".to_string(),
            ColumnKind::Text,
            DisplayHint::Temporal,
        ),
        OracleType::TimestampTz => (
            "TIMESTAMP WITH TIME ZONE".to_string(),
            ColumnKind::Text,
            DisplayHint::Temporal,
        ),
        OracleType::TimestampLtz => (
            "TIMESTAMP WITH LOCAL TIME ZONE".to_string(),
            ColumnKind::Text,
            DisplayHint::Temporal,
        ),
        OracleType::Raw => (
            format!("RAW({})", column.data_size),
            ColumnKind::Bytes,
            DisplayHint::Binary,
        ),
        OracleType::LongRaw | OracleType::Blob | OracleType::Bfile => {
            ("BLOB".to_string(), ColumnKind::Bytes, DisplayHint::Binary)
        }
        OracleType::Boolean => ("BOOLEAN".to_string(), ColumnKind::Bool, DisplayHint::Plain),
        OracleType::Json => ("JSON".to_string(), ColumnKind::Text, DisplayHint::Json),
        OracleType::Rowid => ("ROWID".to_string(), ColumnKind::Text, DisplayHint::Plain),
        OracleType::Urowid => ("UROWID".to_string(), ColumnKind::Text, DisplayHint::Plain),
        OracleType::IntervalYm => (
            "INTERVAL YEAR TO MONTH".to_string(),
            ColumnKind::Text,
            DisplayHint::Interval,
        ),
        OracleType::IntervalDs => (
            "INTERVAL DAY TO SECOND".to_string(),
            ColumnKind::Text,
            DisplayHint::Interval,
        ),
        OracleType::Cursor | OracleType::Object | OracleType::Vector => (
            column
                .type_name
                .clone()
                .unwrap_or_else(|| "OBJECT".to_string()),
            ColumnKind::Text,
            DisplayHint::Plain,
        ),
    };
    ColumnMeta::new(column.name.clone(), type_name, kind, hint)
}

fn number_type(precision: i16, scale: i16) -> String {
    if precision > 0 {
        if scale == 0 {
            format!("NUMBER({precision})")
        } else {
            format!("NUMBER({precision},{scale})")
        }
    } else {
        "NUMBER".to_string()
    }
}

/// An Oracle value as an owned sqlr value. Dates, decimals and ROWIDs travel as text.
pub(crate) fn decode(value: &OracleValue, ty: OracleType) -> Value {
    match (value, ty) {
        (OracleValue::Null, _) => Value::Null,
        (OracleValue::Integer(v), OracleType::BinaryInteger) => Value::Int(*v),
        (OracleValue::Integer(v), OracleType::Number) => Value::Int(*v),
        (OracleValue::Float(v), OracleType::Number) => Value::Text(v.to_string()),
        (OracleValue::Number(v), _) => match v.to_i64() {
            Ok(i) if v.is_integer => Value::Int(i),
            _ => Value::Text(v.value.clone()),
        },
        (OracleValue::Integer(v), OracleType::BinaryFloat | OracleType::BinaryDouble) => {
            Value::Float(*v as f64)
        }
        (OracleValue::Float(v), OracleType::BinaryFloat | OracleType::BinaryDouble) => {
            Value::Float(*v)
        }
        (OracleValue::Date(v), _) => Value::Text(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            v.year, v.month, v.day, v.hour, v.minute, v.second
        )),
        (OracleValue::Timestamp(v), _) => {
            let mut text = format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:06}",
                v.year, v.month, v.day, v.hour, v.minute, v.second, v.microsecond
            );
            if v.has_timezone() {
                let sign = if v.tz_hour_offset < 0 { '-' } else { '+' };
                text.push_str(&format!(
                    " {sign}{:02}:{:02}",
                    v.tz_hour_offset.unsigned_abs(),
                    v.tz_minute_offset.unsigned_abs()
                ));
            }
            Value::Text(text)
        }
        (OracleValue::Bytes(v), _) => Value::Bytes(v.clone()),
        (OracleValue::String(v), _) => Value::Text(v.clone()),
        (OracleValue::Boolean(v), _) => Value::Bool(*v),
        (OracleValue::RowId(v), _) => Value::Text(format!("{v}")),
        (OracleValue::Json(v), _) => Value::Text(v.to_string()),
        (OracleValue::Lob(v), OracleType::Blob | OracleType::Bfile | OracleType::Raw) => {
            match v.as_bytes() {
                Ok(Some(bytes)) => Value::Bytes(bytes.to_vec()),
                _ => Value::Bytes(Vec::new()),
            }
        }
        (OracleValue::Lob(v), _) => Value::Text(lob_text(v)),
        (OracleValue::Cursor(_), _) => Value::Text("[cursor]".to_string()),
        (OracleValue::Collection(_), _) => Value::Text("[collection]".to_string()),
        (OracleValue::Vector(_), _) => Value::Text("[vector]".to_string()),
        (OracleValue::Integer(v), _) => Value::Int(*v),
        (OracleValue::Float(v), _) => Value::Float(*v),
    }
}

fn lob_text(lob: &LobValue) -> String {
    match lob.as_string() {
        Ok(Some(text)) => text,
        Ok(None) => String::new(),
        Err(_) => "[LOB]".to_string(),
    }
}

/// A bound parameter as the value the server gets.
pub(crate) fn encode(value: &Value) -> OracleValue {
    match value {
        Value::Null => OracleValue::Null,
        Value::Bool(v) => OracleValue::Boolean(*v),
        Value::Int(v) => OracleValue::Integer(*v),
        Value::Float(v) => OracleValue::Float(*v),
        Value::Text(v) => OracleValue::String(v.clone()),
        Value::Bytes(v) => OracleValue::Bytes(v.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oracle_rs::types::OracleDate;

    #[test]
    fn number_type_uses_precision_and_scale() {
        assert_eq!(number_type(0, 0), "NUMBER");
        assert_eq!(number_type(10, 0), "NUMBER(10)");
        assert_eq!(number_type(10, 2), "NUMBER(10,2)");
    }

    #[test]
    fn dates_and_numbers_decode_as_text() {
        let date = OracleDate::new(2024, 1, 2, 3, 4, 5);
        assert_eq!(
            decode(&OracleValue::Date(date), OracleType::Date),
            Value::Text("2024-01-02 03:04:05".into())
        );
        assert_eq!(
            decode(&OracleValue::Integer(42), OracleType::Number),
            Value::Int(42)
        );
        assert_eq!(
            decode(&OracleValue::Integer(42), OracleType::BinaryInteger),
            Value::Int(42)
        );
    }
}
