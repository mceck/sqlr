//! HANA types as chunk columns and HANA values as cells.

use hdbconnect_async::{HdbValue, TypeId};
use sqlr_driver_sdk::{ColumnKind, DisplayHint, DriverError, ErrorKind, Value};

use crate::errors;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnShape {
    pub kind: ColumnKind,
    pub hint: DisplayHint,
}

/// How a column of type `id` is stored and shown.
pub fn column_kind(id: TypeId) -> ColumnShape {
    let (kind, hint) = match id {
        TypeId::TINYINT | TypeId::SMALLINT | TypeId::INT | TypeId::BIGINT => {
            (ColumnKind::Int, DisplayHint::Number)
        }
        // Exact numbers travel as text: no precision is lost to a float.
        TypeId::DECIMAL => (ColumnKind::Text, DisplayHint::Decimal),
        TypeId::REAL | TypeId::DOUBLE => (ColumnKind::Float, DisplayHint::Number),
        TypeId::BOOLEAN => (ColumnKind::Bool, DisplayHint::Plain),
        TypeId::CHAR
        | TypeId::VARCHAR
        | TypeId::NCHAR
        | TypeId::NVARCHAR
        | TypeId::STRING
        | TypeId::NSTRING
        | TypeId::CLOB
        | TypeId::NCLOB
        | TypeId::TEXT
        | TypeId::SHORTTEXT
        | TypeId::ALPHANUM => (ColumnKind::Text, DisplayHint::Plain),
        TypeId::BINARY | TypeId::VARBINARY | TypeId::BLOB | TypeId::BSTRING | TypeId::BINTEXT => {
            (ColumnKind::Bytes, DisplayHint::Binary)
        }
        TypeId::LONGDATE | TypeId::SECONDDATE | TypeId::DAYDATE | TypeId::SECONDTIME => {
            (ColumnKind::Text, DisplayHint::Temporal)
        }
        TypeId::GEOMETRY | TypeId::POINT => (ColumnKind::Bytes, DisplayHint::Geometry),
        // Anything else is shown whatever it holds.
        _ => (ColumnKind::Mixed, DisplayHint::Plain),
    };
    ColumnShape { kind, hint }
}

/// The name HANA gives the type, as the grid's header tooltip shows it.
pub(crate) fn type_name(id: TypeId) -> String {
    format!("{id:?}")
}

/// One value, with large objects read whole.
pub(crate) async fn value(v: HdbValue<'static>) -> Result<Value, DriverError> {
    let lob = |e: hdbconnect_async::HdbError| errors::map(e);
    Ok(match v {
        HdbValue::NULL => Value::Null,
        HdbValue::TINYINT(x) => Value::Int(i64::from(x)),
        HdbValue::SMALLINT(x) => Value::Int(i64::from(x)),
        HdbValue::INT(x) => Value::Int(i64::from(x)),
        HdbValue::BIGINT(x) => Value::Int(x),
        HdbValue::DECIMAL(x) => Value::Text(x.to_string()),
        // `1.5f32` shown as `1.5`, not as the f64 nearest to it.
        HdbValue::REAL(x) => Value::Float(x.to_string().parse().unwrap_or(f64::from(x))),
        HdbValue::DOUBLE(x) => Value::Float(x),
        HdbValue::BOOLEAN(x) => Value::Bool(x),
        HdbValue::STRING(x) => Value::Text(x),
        HdbValue::BINARY(x) => Value::Bytes(x),
        HdbValue::ASYNC_BLOB(b) => Value::Bytes(b.into_bytes().await.map_err(lob)?),
        HdbValue::ASYNC_CLOB(c) => Value::Text(c.into_string().await.map_err(lob)?),
        HdbValue::ASYNC_NCLOB(c) => Value::Text(c.into_string().await.map_err(lob)?),
        HdbValue::LONGDATE(x) => Value::Text(x.to_string()),
        HdbValue::SECONDDATE(x) => Value::Text(x.to_string()),
        HdbValue::DAYDATE(x) => Value::Text(x.to_string()),
        HdbValue::SECONDTIME(x) => Value::Text(x.to_string()),
        HdbValue::GEOMETRY(x) | HdbValue::POINT(x) => Value::Bytes(x),
        // Never the value itself in the message: it may be large or private.
        _ => {
            return Err(DriverError::new(
                ErrorKind::Unsupported,
                "a value of a HANA type sqlr does not read yet; cast the column to text or binary",
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ColumnKind::*;
    use DisplayHint::*;

    #[test]
    fn type_mapping_table() {
        let table = [
            (TypeId::TINYINT, Int, Number),
            (TypeId::SMALLINT, Int, Number),
            (TypeId::INT, Int, Number),
            (TypeId::BIGINT, Int, Number),
            (TypeId::DECIMAL, Text, Decimal),
            (TypeId::REAL, Float, Number),
            (TypeId::DOUBLE, Float, Number),
            (TypeId::BOOLEAN, Bool, Plain),
            (TypeId::VARCHAR, Text, Plain),
            (TypeId::NVARCHAR, Text, Plain),
            (TypeId::NCLOB, Text, Plain),
            (TypeId::BLOB, Bytes, Binary),
            (TypeId::VARBINARY, Bytes, Binary),
            (TypeId::DAYDATE, Text, Temporal),
            (TypeId::SECONDTIME, Text, Temporal),
            (TypeId::SECONDDATE, Text, Temporal),
            (TypeId::LONGDATE, Text, Temporal),
            (TypeId::GEOMETRY, Bytes, Geometry),
        ];
        for (id, kind, hint) in table {
            assert_eq!(column_kind(id), ColumnShape { kind, hint }, "{id:?}");
        }
    }

    #[tokio::test]
    async fn values_keep_their_exactness() {
        let big = hdbconnect_async::HdbValue::DECIMAL(bigdecimal_from(
            "1234567890123456789012345678.0123456789",
        ));
        assert_eq!(
            value(big).await.unwrap(),
            Value::Text("1234567890123456789012345678.0123456789".into())
        );
        assert_eq!(value(HdbValue::REAL(1.5)).await.unwrap(), Value::Float(1.5));
        assert_eq!(value(HdbValue::REAL(0.1)).await.unwrap(), Value::Float(0.1));
        assert_eq!(
            value(HdbValue::BIGINT(i64::MAX)).await.unwrap(),
            Value::Int(i64::MAX)
        );
        assert_eq!(value(HdbValue::NULL).await.unwrap(), Value::Null);
        // Spatial columns are bytes (EWKB), not an error that fails the whole query.
        assert_eq!(
            value(HdbValue::GEOMETRY(vec![1, 2, 3])).await.unwrap(),
            Value::Bytes(vec![1, 2, 3])
        );
        assert_eq!(
            value(HdbValue::POINT(vec![9])).await.unwrap(),
            Value::Bytes(vec![9])
        );
        assert_eq!(
            value(HdbValue::STRING("日本語 è 🚀".into())).await.unwrap(),
            Value::Text("日本語 è 🚀".into())
        );
    }

    fn bigdecimal_from(text: &str) -> bigdecimal::BigDecimal {
        text.parse().expect("a decimal")
    }
}
