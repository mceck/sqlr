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
        // oracle-rs hands these over as lossily decoded text, so a cell is a float only
        // when that text parses; the column must accept the text otherwise.
        OracleType::BinaryFloat => (
            "BINARY_FLOAT".to_string(),
            ColumnKind::Mixed,
            DisplayHint::Number,
        ),
        OracleType::BinaryDouble => (
            "BINARY_DOUBLE".to_string(),
            ColumnKind::Mixed,
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
        // Some NUMBERs arrive as text; an integral one is still an integer.
        (OracleValue::String(v), OracleType::Number) => match v.trim().parse::<i64>() {
            Ok(i) => Value::Int(i),
            Err(_) => Value::Text(v.clone()),
        },
        (OracleValue::String(v), OracleType::BinaryFloat | OracleType::BinaryDouble) => {
            match v.trim().parse::<f64>() {
                Ok(f) => Value::Float(f),
                Err(_) => Value::Text(v.clone()),
            }
        }
        (OracleValue::Date(v), _) => Value::Text(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            v.year, v.month, v.day, v.hour, v.minute, v.second
        )),
        (OracleValue::Timestamp(v), ty) => {
            let mut text = format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:06}",
                v.year, v.month, v.day, v.hour, v.minute, v.second, v.microsecond
            );
            // `has_timezone` is false for a UTC offset (`+00:00`): a TIMESTAMP WITH TIME
            // ZONE column keeps its zone in the text even when the offset is zero.
            if v.has_timezone() || ty == OracleType::TimestampTz {
                let sign = if v.tz_hour_offset < 0 || v.tz_minute_offset < 0 {
                    '-'
                } else {
                    '+'
                };
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
        // A NULL LOB is a NULL cell, not an empty string or an empty byte string.
        (OracleValue::Lob(LobValue::Null), _) => Value::Null,
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

    fn col(ty: OracleType) -> ColumnInfo {
        let mut c = ColumnInfo::new("C", ty);
        c.data_size = 30;
        c.precision = 12;
        c.scale = 3;
        c
    }

    #[test]
    fn every_column_type_has_a_meta() {
        let expect = [
            (OracleType::Varchar, "VARCHAR2(30)", ColumnKind::Text),
            (OracleType::Char, "CHAR(30)", ColumnKind::Text),
            (OracleType::Long, "CLOB", ColumnKind::Text),
            (OracleType::Clob, "CLOB", ColumnKind::Text),
            (OracleType::Number, "NUMBER(12,3)", ColumnKind::Mixed),
            (OracleType::BinaryInteger, "BINARY_INTEGER", ColumnKind::Int),
            (OracleType::BinaryFloat, "BINARY_FLOAT", ColumnKind::Mixed),
            (OracleType::BinaryDouble, "BINARY_DOUBLE", ColumnKind::Mixed),
            (OracleType::Date, "DATE", ColumnKind::Text),
            (OracleType::Timestamp, "TIMESTAMP", ColumnKind::Text),
            (
                OracleType::TimestampTz,
                "TIMESTAMP WITH TIME ZONE",
                ColumnKind::Text,
            ),
            (
                OracleType::TimestampLtz,
                "TIMESTAMP WITH LOCAL TIME ZONE",
                ColumnKind::Text,
            ),
            (OracleType::Raw, "RAW(30)", ColumnKind::Bytes),
            (OracleType::LongRaw, "BLOB", ColumnKind::Bytes),
            (OracleType::Blob, "BLOB", ColumnKind::Bytes),
            (OracleType::Bfile, "BLOB", ColumnKind::Bytes),
            (OracleType::Boolean, "BOOLEAN", ColumnKind::Bool),
            (OracleType::Json, "JSON", ColumnKind::Text),
            (OracleType::Rowid, "ROWID", ColumnKind::Text),
            (OracleType::Urowid, "UROWID", ColumnKind::Text),
            (
                OracleType::IntervalYm,
                "INTERVAL YEAR TO MONTH",
                ColumnKind::Text,
            ),
            (
                OracleType::IntervalDs,
                "INTERVAL DAY TO SECOND",
                ColumnKind::Text,
            ),
            (OracleType::Cursor, "OBJECT", ColumnKind::Text),
            (OracleType::Object, "OBJECT", ColumnKind::Text),
            (OracleType::Vector, "OBJECT", ColumnKind::Text),
        ];
        for (ty, name, kind) in expect {
            let meta = column_meta(&col(ty));
            assert_eq!(meta.type_name, name, "{ty:?}");
            assert_eq!(meta.kind, kind, "{ty:?}");
            assert_eq!(meta.name, "C");
        }
    }

    #[test]
    fn object_columns_keep_their_declared_type_name() {
        let mut c = col(OracleType::Object);
        c.type_name = Some("MY_TYPE".into());
        assert_eq!(column_meta(&c).type_name, "MY_TYPE");
    }

    #[test]
    fn numbers_decode_by_shape() {
        use oracle_rs::types::OracleNumber;
        let n = |s: &str| OracleValue::Number(OracleNumber::new(s));
        assert_eq!(decode(&n("42"), OracleType::Number), Value::Int(42));
        assert_eq!(
            decode(&n("1.50"), OracleType::Number),
            Value::Text("1.50".into())
        );
        assert_eq!(
            decode(&OracleValue::Float(2.5), OracleType::Number),
            Value::Text("2.5".into())
        );
        assert_eq!(
            decode(&OracleValue::Integer(3), OracleType::BinaryDouble),
            Value::Float(3.0)
        );
        assert_eq!(
            decode(&OracleValue::Float(0.5), OracleType::BinaryFloat),
            Value::Float(0.5)
        );
        assert_eq!(
            decode(&OracleValue::String("2.5".into()), OracleType::BinaryFloat),
            Value::Float(2.5)
        );
        assert_eq!(
            decode(
                &OracleValue::String("\u{fffd}\0".into()),
                OracleType::BinaryDouble
            ),
            Value::Text("\u{fffd}\0".into())
        );
        assert_eq!(
            decode(&OracleValue::String("12".into()), OracleType::Number),
            Value::Int(12)
        );
        assert_eq!(
            decode(&OracleValue::String("1.5".into()), OracleType::Number),
            Value::Text("1.5".into())
        );
        assert_eq!(
            decode(&OracleValue::Float(0.5), OracleType::Varchar),
            Value::Float(0.5)
        );
        assert_eq!(
            decode(&OracleValue::Integer(7), OracleType::Varchar),
            Value::Int(7)
        );
    }

    #[test]
    fn timestamps_render_with_and_without_zone() {
        use oracle_rs::types::OracleTimestamp;
        let plain = OracleTimestamp::new(2024, 1, 2, 3, 4, 5, 678);
        assert_eq!(
            decode(&OracleValue::Timestamp(plain), OracleType::Timestamp),
            Value::Text("2024-01-02 03:04:05.000678".into())
        );
        let east = OracleTimestamp::with_timezone(2024, 1, 2, 3, 4, 5, 0, 5, 30);
        assert_eq!(
            decode(&OracleValue::Timestamp(east), OracleType::TimestampTz),
            Value::Text("2024-01-02 03:04:05.000000 +05:30".into())
        );
        let west = OracleTimestamp::with_timezone(2024, 1, 2, 3, 4, 5, 0, -8, 0);
        assert_eq!(
            decode(&OracleValue::Timestamp(west), OracleType::TimestampTz),
            Value::Text("2024-01-02 03:04:05.000000 -08:00".into())
        );
    }

    #[test]
    fn simple_values_decode() {
        let d = |v: OracleValue| decode(&v, OracleType::Varchar);
        assert_eq!(d(OracleValue::Null), Value::Null);
        assert_eq!(d(OracleValue::Bytes(vec![1, 2])), Value::Bytes(vec![1, 2]));
        assert_eq!(d(OracleValue::String("x".into())), Value::Text("x".into()));
        assert_eq!(d(OracleValue::Boolean(true)), Value::Bool(true));
        assert_eq!(
            d(OracleValue::Json(serde_json::json!({"a": 1}))),
            Value::Text("{\"a\":1}".into())
        );
        assert!(matches!(
            d(OracleValue::RowId(oracle_rs::types::RowId::new(1, 2, 3, 4))),
            Value::Text(_)
        ));
    }

    #[test]
    fn a_null_lob_is_null_not_empty() {
        // A NULL CLOB/BLOB arrives as `LobValue::Null`; an empty one as `LobValue::Empty`.
        assert_eq!(
            decode(&OracleValue::Lob(LobValue::Null), OracleType::Clob),
            Value::Null
        );
        assert_eq!(
            decode(&OracleValue::Lob(LobValue::Null), OracleType::Blob),
            Value::Null
        );
        assert_eq!(
            decode(&OracleValue::Lob(LobValue::Empty), OracleType::Clob),
            Value::Text(String::new())
        );
        assert_eq!(
            decode(&OracleValue::Lob(LobValue::Empty), OracleType::Blob),
            Value::Bytes(Vec::new())
        );
    }

    #[test]
    fn a_timestamp_zone_keeps_its_sign_and_its_utc_offset() {
        use oracle_rs::types::OracleTimestamp;
        let utc = OracleTimestamp::with_timezone(2024, 1, 2, 3, 4, 5, 0, 0, 0);
        assert_eq!(
            decode(&OracleValue::Timestamp(utc), OracleType::TimestampTz),
            Value::Text("2024-01-02 03:04:05.000000 +00:00".into())
        );
        // A negative offset under one hour keeps its sign.
        let half = OracleTimestamp::with_timezone(2024, 1, 2, 3, 4, 5, 0, 0, -30);
        assert_eq!(
            decode(&OracleValue::Timestamp(half), OracleType::TimestampTz),
            Value::Text("2024-01-02 03:04:05.000000 -00:30".into())
        );
        // A plain TIMESTAMP has no zone, even though its offsets are zero.
        let plain = OracleTimestamp::new(2024, 1, 2, 3, 4, 5, 0);
        assert_eq!(
            decode(&OracleValue::Timestamp(plain), OracleType::Timestamp),
            Value::Text("2024-01-02 03:04:05.000000".into())
        );
    }

    #[test]
    fn parameters_encode_one_to_one() {
        assert!(matches!(encode(&Value::Null), OracleValue::Null));
        assert!(matches!(
            encode(&Value::Bool(true)),
            OracleValue::Boolean(true)
        ));
        assert!(matches!(encode(&Value::Int(5)), OracleValue::Integer(5)));
        assert!(matches!(encode(&Value::Float(1.5)), OracleValue::Float(f) if f == 1.5));
        assert!(matches!(encode(&Value::Text("a".into())), OracleValue::String(s) if s == "a"));
        assert!(matches!(encode(&Value::Bytes(vec![9])), OracleValue::Bytes(b) if b == [9]));
    }
}
