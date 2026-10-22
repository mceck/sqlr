//! PostgreSQL column types, text-format cell decoding and bound parameters.

use std::borrow::Cow;
use std::error::Error;

use bytes::BytesMut;
use sqlr_core::{Cell, ColumnKind, ColumnMeta, DisplayHint, DriverError, ErrorKind, Value};
use tokio_postgres::types::{Format, IsNull, Kind, ToSql, Type, to_sql_checked};

pub(crate) fn column_meta(ty: &Type, name: &str) -> ColumnMeta {
    let (kind, hint) = match *ty {
        Type::BOOL => (ColumnKind::Bool, DisplayHint::Plain),
        Type::INT2 | Type::INT4 | Type::INT8 | Type::OID => (ColumnKind::Int, DisplayHint::Plain),
        Type::FLOAT4 | Type::FLOAT8 => (ColumnKind::Float, DisplayHint::Plain),
        Type::NUMERIC => (ColumnKind::Text, DisplayHint::Decimal),
        Type::DATE | Type::TIME | Type::TIMETZ | Type::TIMESTAMP | Type::TIMESTAMPTZ => {
            (ColumnKind::Text, DisplayHint::Temporal)
        }
        Type::INTERVAL => (ColumnKind::Text, DisplayHint::Interval),
        Type::JSON | Type::JSONB => (ColumnKind::Text, DisplayHint::Json),
        Type::XML => (ColumnKind::Text, DisplayHint::Xml),
        Type::UUID => (ColumnKind::Text, DisplayHint::Uuid),
        Type::BYTEA => (ColumnKind::Bytes, DisplayHint::Binary),
        _ if matches!(ty.kind(), Kind::Array(_)) => (ColumnKind::Text, DisplayHint::Array),
        // PostGIS types have no fixed oid.
        _ if matches!(ty.name(), "geometry" | "geography") => {
            (ColumnKind::Text, DisplayHint::Geometry)
        }
        _ => (ColumnKind::Text, DisplayHint::Plain),
    };
    ColumnMeta::new(name, ty.name(), kind, hint)
}

/// Decodes one value in PostgreSQL's text format. `bytea` goes through [`parse_bytea`].
pub(crate) fn parse_cell<'a>(
    kind: ColumnKind,
    text: Option<&'a str>,
) -> Result<Cell<'a>, DriverError> {
    let Some(text) = text else {
        return Ok(Cell::Null);
    };
    let invalid = || {
        DriverError::new(
            ErrorKind::Protocol,
            format!("invalid {kind:?} value `{text}`"),
        )
    };
    Ok(match kind {
        ColumnKind::Bool => match text {
            "t" => Cell::Bool(true),
            "f" => Cell::Bool(false),
            _ => return Err(invalid()),
        },
        ColumnKind::Int => Cell::Int(text.parse().map_err(|_| invalid())?),
        ColumnKind::Float => Cell::Float(match text {
            "NaN" => f64::NAN,
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            _ => text.parse().map_err(|_| invalid())?,
        }),
        ColumnKind::Text => Cell::Text(text),
        ColumnKind::Bytes | ColumnKind::Mixed => {
            return Err(DriverError::new(
                ErrorKind::Protocol,
                "bytea values are decoded into a buffer; PostgreSQL columns are never Mixed",
            ));
        }
    })
}

/// Decodes `bytea` text output into `out` (cleared first): the hex format (`\x0aff`) or the
/// legacy escape format (`\\` and `\ooo` octal escapes).
pub(crate) fn parse_bytea(text: &str, out: &mut Vec<u8>) -> Result<(), DriverError> {
    out.clear();
    let invalid = || {
        let shown: String = text.chars().take(40).collect();
        DriverError::new(
            ErrorKind::Protocol,
            format!("invalid bytea value `{shown}`"),
        )
    };
    if let Some(hex) = text.strip_prefix("\\x") {
        let digit = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
        let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
        if !rest.is_empty() {
            return Err(invalid());
        }
        out.reserve(pairs.len());
        for [hi, lo] in pairs {
            let (Some(hi), Some(lo)) = (digit(*hi), digit(*lo)) else {
                return Err(invalid());
            };
            out.push(hi << 4 | lo);
        }
        return Ok(());
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
        } else if bytes.get(i + 1) == Some(&b'\\') {
            out.push(b'\\');
            i += 2;
        } else {
            let octal = bytes.get(i + 1..i + 4).ok_or_else(invalid)?;
            let s = std::str::from_utf8(octal).map_err(|_| invalid())?;
            out.push(u8::from_str_radix(s, 8).map_err(|_| invalid())?);
            i += 4;
        }
    }
    Ok(())
}

/// A bound parameter. Its encoding follows the parameter type the server inferred: binary
/// where the value converts losslessly, PostgreSQL's text input otherwise (numeric, dates,
/// UUIDs, JSON, enums, arrays, …), so the server parses and validates it.
#[derive(Debug)]
pub(crate) struct Param<'a>(pub(crate) &'a Value);

enum Encoding<'a> {
    Null,
    Bool(bool),
    Int2(i16),
    Int4(i32),
    Int8(i64),
    Float8(f64),
    Bytes(&'a [u8]),
    Text(Cow<'a, str>),
}

impl Param<'_> {
    fn encoding(&self, ty: &Type) -> Encoding<'_> {
        match (self.0, ty) {
            (Value::Null, _) => Encoding::Null,
            (Value::Bool(v), &Type::BOOL) => Encoding::Bool(*v),
            (Value::Int(v), &Type::INT8) => Encoding::Int8(*v),
            (Value::Int(v), &Type::INT4) if i32::try_from(*v).is_ok() => Encoding::Int4(*v as i32),
            (Value::Int(v), &Type::INT2) if i16::try_from(*v).is_ok() => Encoding::Int2(*v as i16),
            (Value::Float(v), &Type::FLOAT8) => Encoding::Float8(*v),
            (Value::Bytes(v), &Type::BYTEA) => Encoding::Bytes(v),
            (value, _) => Encoding::Text(text_input(value)),
        }
    }
}

/// `value` in PostgreSQL's text input syntax.
fn text_input(value: &Value) -> Cow<'_, str> {
    match value {
        Value::Bool(v) => Cow::Borrowed(if *v { "true" } else { "false" }),
        Value::Float(v) if v.is_nan() => Cow::Borrowed("NaN"),
        Value::Float(v) if v.is_infinite() => {
            Cow::Borrowed(if *v > 0.0 { "Infinity" } else { "-Infinity" })
        }
        // `bytea`'s hex input; a bytes value bound to another type is rejected by the server.
        Value::Bytes(v) => Cow::Owned(format!("\\x{}", &sqlr_core::hex_bytes(v)[2..])),
        other => other.display(),
    }
}

impl ToSql for Param<'_> {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        match self.encoding(ty) {
            Encoding::Null => return Ok(IsNull::Yes),
            Encoding::Bool(v) => v.to_sql(ty, out)?,
            Encoding::Int2(v) => v.to_sql(ty, out)?,
            Encoding::Int4(v) => v.to_sql(ty, out)?,
            Encoding::Int8(v) => v.to_sql(ty, out)?,
            Encoding::Float8(v) => v.to_sql(ty, out)?,
            Encoding::Bytes(v) => {
                out.extend_from_slice(v);
                IsNull::No
            }
            Encoding::Text(v) => {
                out.extend_from_slice(v.as_bytes());
                IsNull::No
            }
        };
        Ok(IsNull::No)
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    fn encode_format(&self, ty: &Type) -> Format {
        match self.encoding(ty) {
            Encoding::Text(_) => Format::Text,
            _ => Format::Binary,
        }
    }

    to_sql_checked!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_meta_maps_kinds_and_hints() {
        let m = column_meta(&Type::OID, "o");
        assert_eq!((m.kind, m.hint), (ColumnKind::Int, DisplayHint::Plain));
        assert_eq!((m.name.as_str(), m.type_name.as_str()), ("o", "oid"));
        let cases = [
            (Type::INTERVAL, ColumnKind::Text, DisplayHint::Interval),
            (Type::UUID, ColumnKind::Text, DisplayHint::Uuid),
            (Type::NUMERIC, ColumnKind::Text, DisplayHint::Decimal),
            (Type::XML, ColumnKind::Text, DisplayHint::Xml),
            (Type::INT4_ARRAY, ColumnKind::Text, DisplayHint::Array),
            (Type::BYTEA, ColumnKind::Bytes, DisplayHint::Binary),
            (Type::TIMESTAMPTZ, ColumnKind::Text, DisplayHint::Temporal),
        ];
        for (ty, kind, hint) in cases {
            let m = column_meta(&ty, "x");
            assert_eq!((m.kind, m.hint), (kind, hint), "{ty}");
        }
        let geometry = Type::new("geometry".into(), 90_000, Kind::Simple, "public".into());
        assert_eq!(column_meta(&geometry, "g").hint, DisplayHint::Geometry);
    }

    #[test]
    fn parse_cell_text_format() {
        assert_eq!(parse_cell(ColumnKind::Int, None).unwrap(), Cell::Null);
        assert_eq!(
            parse_cell(ColumnKind::Bool, Some("f")).unwrap(),
            Cell::Bool(false)
        );
        assert_eq!(
            parse_cell(ColumnKind::Int, Some("-42")).unwrap(),
            Cell::Int(-42)
        );
        assert_eq!(
            parse_cell(ColumnKind::Float, Some("-Infinity")).unwrap(),
            Cell::Float(f64::NEG_INFINITY)
        );
        assert_eq!(
            parse_cell(ColumnKind::Float, Some("2.5e-3")).unwrap(),
            Cell::Float(0.0025)
        );
    }

    #[test]
    fn parse_failure_is_protocol_error() {
        for (kind, text) in [
            (ColumnKind::Bool, "true"),
            (ColumnKind::Int, "1.5"),
            (ColumnKind::Float, "abc"),
            (ColumnKind::Bytes, "\\x00"),
        ] {
            let err = parse_cell(kind, Some(text)).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Protocol, "{kind:?} {text}");
        }
    }

    #[test]
    fn bytea_hex_and_escape_formats() {
        let mut out = vec![9];
        parse_bytea("\\x00aBff", &mut out).unwrap();
        assert_eq!(out, [0x00, 0xab, 0xff]);
        parse_bytea("\\x", &mut out).unwrap();
        assert!(out.is_empty());
        parse_bytea(r"a\\b\000\377", &mut out).unwrap();
        assert_eq!(out, [b'a', b'\\', b'b', 0, 0xff]);
        for bad in ["\\x0", "\\xzz", r"\12", r"\9zz"] {
            assert!(parse_bytea(bad, &mut out).is_err(), "{bad}");
        }
    }

    #[derive(Debug, PartialEq)]
    enum Wire {
        Binary,
        Text,
    }

    fn encoded(value: Value, ty: Type) -> (Wire, Option<Vec<u8>>) {
        let param = Param(&value);
        let mut out = BytesMut::new();
        let is_null = param.to_sql_checked(&ty, &mut out).unwrap();
        let bytes = matches!(is_null, IsNull::No).then(|| out.to_vec());
        let wire = match param.encode_format(&ty) {
            Format::Text => Wire::Text,
            Format::Binary => Wire::Binary,
        };
        (wire, bytes)
    }

    #[test]
    fn params_follow_the_target_type() {
        use Wire::{Binary, Text};
        assert_eq!(encoded(Value::Null, Type::UUID).1, None);
        assert_eq!(
            encoded(Value::Int(7), Type::INT2),
            (Binary, Some(vec![0, 7]))
        );
        // Out of range for the binary form: the server reports the overflow.
        assert_eq!(
            encoded(Value::Int(70_000), Type::INT2),
            (Text, Some(b"70000".to_vec()))
        );
        assert_eq!(
            encoded(Value::Int(5), Type::NUMERIC),
            (Text, Some(b"5".to_vec()))
        );
        assert_eq!(
            encoded(Value::Bool(true), Type::BOOL),
            (Binary, Some(vec![1]))
        );
        assert_eq!(
            encoded(Value::Bool(false), Type::TEXT),
            (Text, Some(b"false".to_vec()))
        );
        assert_eq!(
            encoded(Value::Float(f64::NEG_INFINITY), Type::FLOAT4),
            (Text, Some(b"-Infinity".to_vec()))
        );
        assert_eq!(
            encoded(Value::Bytes(vec![1, 2]), Type::BYTEA),
            (Binary, Some(vec![1, 2]))
        );
        assert_eq!(
            encoded(Value::Bytes(vec![1, 0xab]), Type::TEXT),
            (Text, Some(b"\\x01ab".to_vec()))
        );
        assert_eq!(
            encoded(Value::Text("12.5".into()), Type::NUMERIC),
            (Text, Some(b"12.5".to_vec()))
        );
    }
}
