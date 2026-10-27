//! MySQL column types and cell decoding, for the text and the binary protocol.

use mysql_async::consts::{ColumnFlags, ColumnType};
use mysql_async::{Column, Value};
use sqlr_core::{
    Cell, ColumnKind, ColumnMeta, ColumnOrigin, DisplayHint, DriverError, ErrorKind, TableRef,
};

/// The `binary` character set: the column holds bytes, not text.
const BINARY_CHARSET: u16 = 63;

/// How the values of one column are decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decode {
    Int,
    /// `BIGINT UNSIGNED`: may not fit an `i64`, carried as text.
    Unsigned,
    Float,
    /// `BIT(1)`.
    Bool,
    /// `BIT(n)`, big-endian; `wide` (more than 63 bits) is carried as text.
    Bits {
        wide: bool,
    },
    Text,
    Date,
    /// `DATETIME`/`TIMESTAMP` with `decimals` fractional digits.
    DateTime {
        decimals: u8,
    },
    Time {
        decimals: u8,
    },
    Bytes,
}

/// The column's metadata (with its origin table, when the server reports one) and decoder.
pub(crate) fn column(col: &Column) -> (ColumnMeta, Decode) {
    use ColumnType::*;
    use DisplayHint as H;
    let flags = col.flags();
    let binary = col.character_set() == BINARY_CHARSET;
    let unsigned = flags.contains(ColumnFlags::UNSIGNED_FLAG);
    let decimals = col.decimals().min(6);
    let int = |name: &str| -> (String, ColumnKind, DisplayHint, Decode) {
        let name = if unsigned {
            format!("{name} unsigned")
        } else {
            name.to_string()
        };
        (name, ColumnKind::Int, H::Plain, Decode::Int)
    };
    let text = |name: &str, hint| (name.to_string(), ColumnKind::Text, hint, Decode::Text);
    let bytes = |name: &str, hint| (name.to_string(), ColumnKind::Bytes, hint, Decode::Bytes);
    let (type_name, kind, hint, decode) = match col.column_type() {
        MYSQL_TYPE_TINY => int("tinyint"),
        MYSQL_TYPE_SHORT => int("smallint"),
        MYSQL_TYPE_INT24 => int("mediumint"),
        MYSQL_TYPE_LONG => int("int"),
        MYSQL_TYPE_LONGLONG if unsigned => (
            "bigint unsigned".to_string(),
            ColumnKind::Text,
            H::Decimal,
            Decode::Unsigned,
        ),
        MYSQL_TYPE_LONGLONG => int("bigint"),
        MYSQL_TYPE_YEAR => ("year".into(), ColumnKind::Int, H::Plain, Decode::Int),
        MYSQL_TYPE_FLOAT => ("float".into(), ColumnKind::Float, H::Plain, Decode::Float),
        MYSQL_TYPE_DOUBLE => ("double".into(), ColumnKind::Float, H::Plain, Decode::Float),
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => text("decimal", H::Decimal),
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => {
            ("date".into(), ColumnKind::Text, H::Temporal, Decode::Date)
        }
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => (
            "datetime".into(),
            ColumnKind::Text,
            H::Temporal,
            Decode::DateTime { decimals },
        ),
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => (
            "timestamp".into(),
            ColumnKind::Text,
            H::Temporal,
            Decode::DateTime { decimals },
        ),
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => (
            "time".into(),
            ColumnKind::Text,
            H::Temporal,
            Decode::Time { decimals },
        ),
        MYSQL_TYPE_BIT if col.column_length() <= 1 => {
            ("bit".into(), ColumnKind::Bool, H::Plain, Decode::Bool)
        }
        MYSQL_TYPE_BIT if col.column_length() <= 63 => (
            "bit".into(),
            ColumnKind::Int,
            H::Plain,
            Decode::Bits { wide: false },
        ),
        MYSQL_TYPE_BIT => (
            "bit".into(),
            ColumnKind::Text,
            H::Decimal,
            Decode::Bits { wide: true },
        ),
        MYSQL_TYPE_JSON => text("json", H::Json),
        MYSQL_TYPE_GEOMETRY => bytes("geometry", H::Geometry),
        MYSQL_TYPE_VECTOR => bytes("vector", H::Binary),
        MYSQL_TYPE_ENUM => text("enum", H::Plain),
        MYSQL_TYPE_SET => text("set", H::Plain),
        MYSQL_TYPE_STRING | MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_VARCHAR => {
            let fixed = col.column_type() == MYSQL_TYPE_STRING;
            if flags.contains(ColumnFlags::ENUM_FLAG) {
                text("enum", H::Plain)
            } else if flags.contains(ColumnFlags::SET_FLAG) {
                text("set", H::Plain)
            } else if binary {
                bytes(if fixed { "binary" } else { "varbinary" }, H::Binary)
            } else {
                text(if fixed { "char" } else { "varchar" }, H::Plain)
            }
        }
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            if binary {
                bytes("blob", H::Binary)
            } else {
                text("text", H::Plain)
            }
        }
        MYSQL_TYPE_NULL => text("null", H::Plain),
        _ if binary => bytes("binary", H::Binary),
        _ => text("unknown", H::Plain),
    };
    let meta = ColumnMeta::new(col.name_str(), type_name, kind, hint).with_origin(origin(col));
    (meta, decode)
}

/// The table column a result column was read from: MySQL databases are sqlr's schema level,
/// so the database goes in [`TableRef::schema`].
fn origin(col: &Column) -> Option<ColumnOrigin> {
    let table = col.org_table_str();
    let column = col.org_name_str();
    if table.is_empty() || column.is_empty() {
        return None;
    }
    let schema = col.schema_str();
    Some(ColumnOrigin {
        table: TableRef {
            database: None,
            schema: (!schema.is_empty()).then(|| schema.into_owned()),
            name: table.into_owned(),
        },
        column: column.into_owned(),
    })
}

/// A decoded cell, borrowed from the row when it can be.
pub(crate) enum Decoded<'a> {
    Cell(Cell<'a>),
    Owned(String),
}

impl Decoded<'_> {
    pub(crate) fn cell(&self) -> Cell<'_> {
        match self {
            Decoded::Cell(c) => *c,
            Decoded::Owned(s) => Cell::Text(s),
        }
    }
}

fn invalid(decode: Decode, value: &Value) -> DriverError {
    DriverError::new(
        ErrorKind::Protocol,
        format!("cannot decode {value:?} as {decode:?}"),
    )
}

/// Text from the server: borrowed when it is valid UTF-8, else lossily converted.
fn text(bytes: &[u8]) -> Decoded<'_> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Decoded::Cell(Cell::Text(s)),
        Err(_) => Decoded::Owned(String::from_utf8_lossy(bytes).into_owned()),
    }
}

fn big_endian(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

fn fraction(micros: u32, decimals: u8) -> String {
    if decimals == 0 {
        return String::new();
    }
    let digits = format!("{micros:06}");
    format!(".{}", &digits[..usize::from(decimals)])
}

/// Decodes one value. Text-protocol values are all `Bytes` (the text the server prints);
/// binary-protocol values are typed.
pub(crate) fn decode(decode: Decode, value: &Value) -> Result<Decoded<'_>, DriverError> {
    use Decode as D;
    let cell = |c| Ok(Decoded::Cell(c));
    match (decode, value) {
        (_, Value::NULL) => cell(Cell::Null),
        (D::Int, Value::Int(i)) => cell(Cell::Int(*i)),
        (D::Int, Value::UInt(u)) => match i64::try_from(*u) {
            Ok(i) => cell(Cell::Int(i)),
            Err(_) => Err(invalid(decode, value)),
        },
        (D::Int, Value::Bytes(b)) => match std::str::from_utf8(b).ok().and_then(|s| s.parse().ok())
        {
            Some(i) => cell(Cell::Int(i)),
            None => Err(invalid(decode, value)),
        },
        (D::Unsigned, Value::UInt(u)) => Ok(Decoded::Owned(u.to_string())),
        (D::Unsigned, Value::Int(i)) => Ok(Decoded::Owned(i.to_string())),
        (D::Float, Value::Double(f)) => cell(Cell::Float(*f)),
        // Through its shortest decimal form, so a FLOAT reads the same in both protocols.
        (D::Float, Value::Float(f)) => cell(Cell::Float(f.to_string().parse().unwrap_or(f64::NAN))),
        (D::Float, Value::Bytes(b)) => {
            match std::str::from_utf8(b).ok().and_then(|s| s.parse().ok()) {
                Some(f) => cell(Cell::Float(f)),
                None => Err(invalid(decode, value)),
            }
        }
        (D::Bool, Value::Bytes(b)) => cell(Cell::Bool(b.iter().any(|&x| x != 0))),
        (D::Bool, Value::Int(i)) => cell(Cell::Bool(*i != 0)),
        (D::Bool, Value::UInt(u)) => cell(Cell::Bool(*u != 0)),
        (D::Bits { wide }, Value::Bytes(b)) => {
            let n = big_endian(b);
            if wide {
                Ok(Decoded::Owned(n.to_string()))
            } else {
                cell(Cell::Int(n as i64))
            }
        }
        (D::Bits { .. }, Value::Int(_) | Value::UInt(_)) => {
            let n = match value {
                Value::Int(i) => *i as u64,
                Value::UInt(u) => *u,
                _ => unreachable!(),
            };
            match decode {
                D::Bits { wide: true } => Ok(Decoded::Owned(n.to_string())),
                _ => cell(Cell::Int(n as i64)),
            }
        }
        (D::Bytes, Value::Bytes(b)) => cell(Cell::Bytes(b)),
        (D::Date, Value::Date(y, m, d, ..)) => Ok(Decoded::Owned(format!("{y:04}-{m:02}-{d:02}"))),
        (D::DateTime { decimals }, Value::Date(y, mo, d, h, mi, s, us)) => {
            Ok(Decoded::Owned(format!(
                "{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}{}",
                fraction(*us, decimals)
            )))
        }
        (D::Time { decimals }, Value::Time(neg, days, h, mi, s, us)) => {
            let sign = if *neg { "-" } else { "" };
            let hours = days * 24 + u32::from(*h);
            Ok(Decoded::Owned(format!(
                "{sign}{hours:02}:{mi:02}:{s:02}{}",
                fraction(*us, decimals)
            )))
        }
        (_, Value::Bytes(b)) => Ok(text(b)),
        (D::Text, Value::Int(i)) => Ok(Decoded::Owned(i.to_string())),
        (D::Text, Value::UInt(u)) => Ok(Decoded::Owned(u.to_string())),
        (D::Text, Value::Double(f)) => Ok(Decoded::Owned(f.to_string())),
        (D::Text, Value::Float(f)) => Ok(Decoded::Owned(f.to_string())),
        _ => Err(invalid(decode, value)),
    }
}

/// A parameter for a prepared statement.
pub(crate) fn param(value: &sqlr_core::Value) -> Value {
    use sqlr_core::Value as V;
    match value {
        V::Null => Value::NULL,
        V::Bool(b) => Value::Int(i64::from(*b)),
        V::Int(i) => Value::Int(*i),
        V::Float(f) => Value::Double(*f),
        V::Text(s) => Value::Bytes(s.as_bytes().to_vec()),
        V::Bytes(b) => Value::Bytes(b.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(d: Decode, v: &Value) -> String {
        match decode(d, v) {
            Ok(x) => format!("{:?}", x.cell()),
            Err(e) => format!("error: {}", e.message),
        }
    }

    #[test]
    fn decodes_text_protocol() {
        let b = |s: &str| Value::Bytes(s.as_bytes().to_vec());
        assert_eq!(cell(Decode::Int, &b("-42")), "Int(-42)");
        assert_eq!(cell(Decode::Float, &b("1e3")), "Float(1000.0)");
        assert_eq!(
            cell(Decode::Unsigned, &b("18446744073709551615")),
            "Text(\"18446744073709551615\")"
        );
        assert_eq!(cell(Decode::Bool, &Value::Bytes(vec![1])), "Bool(true)");
        assert_eq!(
            cell(Decode::Bits { wide: false }, &Value::Bytes(vec![1, 2])),
            "Int(258)"
        );
        assert_eq!(
            cell(Decode::Bits { wide: true }, &Value::Bytes(vec![0xff; 8])),
            "Text(\"18446744073709551615\")"
        );
        assert_eq!(
            cell(Decode::Bytes, &Value::Bytes(vec![0, 1])),
            "Bytes([0, 1])"
        );
        assert_eq!(
            cell(Decode::Text, &Value::Bytes(vec![b'a', 0xff])),
            "Text(\"a\u{fffd}\")"
        );
        assert_eq!(
            cell(
                Decode::DateTime { decimals: 3 },
                &b("2024-01-02 03:04:05.123")
            ),
            "Text(\"2024-01-02 03:04:05.123\")"
        );
        assert_eq!(cell(Decode::Text, &Value::NULL), "Null");
        assert!(cell(Decode::Int, &b("x")).starts_with("error"));
    }

    #[test]
    fn decodes_binary_protocol() {
        assert_eq!(cell(Decode::Int, &Value::UInt(7)), "Int(7)");
        assert!(cell(Decode::Int, &Value::UInt(u64::MAX)).starts_with("error"));
        assert_eq!(cell(Decode::Float, &Value::Float(1.1)), "Float(1.1)");
        assert_eq!(
            cell(Decode::Unsigned, &Value::UInt(u64::MAX)),
            "Text(\"18446744073709551615\")"
        );
        assert_eq!(
            cell(Decode::Date, &Value::Date(2024, 1, 2, 0, 0, 0, 0)),
            "Text(\"2024-01-02\")"
        );
        assert_eq!(
            cell(
                Decode::DateTime { decimals: 3 },
                &Value::Date(2024, 1, 2, 3, 4, 5, 123_000)
            ),
            "Text(\"2024-01-02 03:04:05.123\")"
        );
        assert_eq!(
            cell(
                Decode::DateTime { decimals: 0 },
                &Value::Date(2024, 1, 2, 3, 4, 5, 0)
            ),
            "Text(\"2024-01-02 03:04:05\")"
        );
        assert_eq!(
            cell(
                Decode::Time { decimals: 0 },
                &Value::Time(true, 1, 2, 3, 4, 0)
            ),
            "Text(\"-26:03:04\")"
        );
        assert_eq!(cell(Decode::Text, &Value::Int(5)), "Text(\"5\")");
    }

    #[test]
    fn params_from_values() {
        use sqlr_core::Value as V;
        assert_eq!(param(&V::Null), Value::NULL);
        assert_eq!(param(&V::Bool(true)), Value::Int(1));
        assert_eq!(
            param(&V::Text("è".into())),
            Value::Bytes("è".as_bytes().to_vec())
        );
        assert_eq!(param(&V::Float(0.5)), Value::Double(0.5));
    }
}
