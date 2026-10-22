//! Owned cell values: query parameters, changesets and the text encoders.

use std::borrow::Cow;

use crate::chunk::{Cell, hex_bytes};

/// An owned cell. Decimals, dates, UUIDs and the like travel as `Text`; the column's
/// [`DisplayHint`](crate::DisplayHint) says what the text holds.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

impl Value {
    pub fn from_cell(cell: Cell<'_>) -> Self {
        match cell {
            Cell::Null => Value::Null,
            Cell::Bool(v) => Value::Bool(v),
            Cell::Int(v) => Value::Int(v),
            Cell::Float(v) => Value::Float(v),
            Cell::Text(v) => Value::Text(v.to_string()),
            Cell::Bytes(v) => Value::Bytes(v.to_vec()),
        }
    }

    pub fn as_cell(&self) -> Cell<'_> {
        match self {
            Value::Null => Cell::Null,
            Value::Bool(v) => Cell::Bool(*v),
            Value::Int(v) => Cell::Int(*v),
            Value::Float(v) => Cell::Float(*v),
            Value::Text(v) => Cell::Text(v),
            Value::Bytes(v) => Cell::Bytes(v),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The full value as text: `NULL`, numbers in Rust's formatting, bytes as `0x…` hex.
    pub fn display(&self) -> Cow<'_, str> {
        match self {
            Value::Text(v) => Cow::Borrowed(v),
            Value::Null => Cow::Borrowed("NULL"),
            Value::Bytes(v) => Cow::Owned(hex_bytes(v)),
            other => Cow::Owned(other.as_cell().to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_from_every_cell_kind() {
        assert_eq!(Value::from_cell(Cell::Null), Value::Null);
        assert_eq!(Value::from_cell(Cell::Bool(true)), Value::Bool(true));
        assert_eq!(Value::from_cell(Cell::Int(-3)), Value::Int(-3));
        assert_eq!(Value::from_cell(Cell::Float(1.5)), Value::Float(1.5));
        assert_eq!(
            Value::from_cell(Cell::Text("è😀")),
            Value::Text("è😀".into())
        );
        assert_eq!(
            Value::from_cell(Cell::Bytes(&[1, 2])),
            Value::Bytes(vec![1, 2])
        );
        for v in [
            Value::Null,
            Value::Bool(false),
            Value::Int(7),
            Value::Float(-0.5),
            Value::Text(String::new()),
            Value::Bytes(Vec::new()),
        ] {
            assert_eq!(Value::from_cell(v.as_cell()), v);
        }
        assert_eq!(Value::Null.display(), "NULL");
        assert_eq!(Value::Int(-3).display(), "-3");
        assert_eq!(Value::Bool(true).display(), "true");
        assert_eq!(Value::Bytes(vec![1, 0xab]).display(), "0x01ab");
        assert!(matches!(
            Value::Text("x".into()).display(),
            Cow::Borrowed("x")
        ));
    }
}
