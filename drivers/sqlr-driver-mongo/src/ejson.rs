//! Conversions between BSON and [`DocValue`].
//!
//! Documents go to the server as raw BSON built from `DocValue`s, so key order and repeated
//! keys survive. Types `DocValue` does not model come back as their Extended JSON shape:
//! JavaScript code as `{"$code": …}` (with `"$scope"`), a DBPointer as `{"$dbPointer": …}`,
//! a symbol as `{"$symbol": …}`. `DocValue::unmodelled` names them, so such a document is
//! never saved back.

use bson::oid::ObjectId;
use bson::raw::{CString, RawArrayBuf, RawBson, RawBsonRef, RawDocument, RawDocumentBuf};
use bson::spec::BinarySubtype;
use bson::{Binary, Decimal128, Document, Regex, Timestamp};
use sqlr_core::{DocValue, DriverError, ErrorKind};

fn invalid(message: String) -> DriverError {
    DriverError::new(ErrorKind::Syntax, message)
}

fn cstring(s: &str, what: &str) -> Result<CString, DriverError> {
    CString::try_from(s).map_err(|_| invalid(format!("{what} \"{s}\" contains a NUL character")))
}

/// `v` as raw BSON.
pub(crate) fn to_raw(v: &DocValue) -> Result<RawBson, DriverError> {
    Ok(match v {
        DocValue::Null => RawBson::Null,
        DocValue::Bool(b) => RawBson::Boolean(*b),
        DocValue::Int32(i) => RawBson::Int32(*i),
        DocValue::Int64(i) => RawBson::Int64(*i),
        DocValue::Double(d) => RawBson::Double(*d),
        DocValue::Decimal128(s) => RawBson::Decimal128(
            s.parse::<Decimal128>()
                .map_err(|_| invalid(format!("invalid decimal \"{s}\"")))?,
        ),
        DocValue::String(s) => RawBson::String(s.clone()),
        DocValue::ObjectId(b) => RawBson::ObjectId(ObjectId::from_bytes(*b)),
        DocValue::DateTime(ms) => RawBson::DateTime(bson::DateTime::from_millis(*ms)),
        DocValue::Binary { subtype, bytes } => RawBson::Binary(Binary {
            subtype: BinarySubtype::from(*subtype),
            bytes: bytes.clone(),
        }),
        DocValue::Regex { pattern, options } => RawBson::RegularExpression(Regex {
            pattern: cstring(pattern, "regex")?,
            options: cstring(options, "regex options")?,
        }),
        DocValue::Timestamp { t, i } => RawBson::Timestamp(Timestamp {
            time: *t,
            increment: *i,
        }),
        DocValue::MinKey => RawBson::MinKey,
        DocValue::MaxKey => RawBson::MaxKey,
        DocValue::Undefined => RawBson::Undefined,
        DocValue::Array(items) => {
            let mut arr = RawArrayBuf::new();
            for item in items {
                arr.push(to_raw(item)?);
            }
            RawBson::Array(arr)
        }
        DocValue::Document(_) => RawBson::Document(to_raw_doc(v)?),
    })
}

/// A document as raw BSON; anything else is an error.
pub(crate) fn to_raw_doc(v: &DocValue) -> Result<RawDocumentBuf, DriverError> {
    let DocValue::Document(fields) = v else {
        return Err(invalid(format!(
            "expected a document, not {}",
            v.type_name()
        )));
    };
    let mut doc = RawDocumentBuf::new();
    for (k, item) in fields {
        doc.append(cstring(k, "field name")?, to_raw(item)?);
    }
    Ok(doc)
}

pub(crate) fn from_raw_doc(doc: &RawDocument) -> Result<DocValue, DriverError> {
    let mut fields = Vec::new();
    for item in doc.iter() {
        let (k, v) = item.map_err(bson_error)?;
        fields.push((k.as_str().to_string(), from_raw(v)?));
    }
    Ok(DocValue::Document(fields))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` function pointer, which receives the error by value"
)]
fn bson_error(e: bson::error::Error) -> DriverError {
    DriverError::new(
        ErrorKind::Protocol,
        format!("invalid BSON from the server: {e}"),
    )
}

/// Plain JSON (a serde value) as a `DocValue`, for the types read through serde.
fn from_json(v: &serde_json::Value) -> DocValue {
    match v {
        serde_json::Value::Null => DocValue::Null,
        serde_json::Value::Bool(b) => DocValue::Bool(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => match i32::try_from(i) {
                Ok(i) => DocValue::Int32(i),
                Err(_) => DocValue::Int64(i),
            },
            None => DocValue::Double(n.as_f64().unwrap_or(f64::NAN)),
        },
        serde_json::Value::String(s) => DocValue::String(s.clone()),
        serde_json::Value::Array(items) => DocValue::Array(items.iter().map(from_json).collect()),
        serde_json::Value::Object(map) => {
            DocValue::Document(map.iter().map(|(k, v)| (k.clone(), from_json(v))).collect())
        }
    }
}

pub(crate) fn from_raw(v: RawBsonRef<'_>) -> Result<DocValue, DriverError> {
    Ok(match v {
        RawBsonRef::Double(d) => DocValue::Double(d),
        RawBsonRef::String(s) => DocValue::String(s.to_string()),
        RawBsonRef::Array(arr) => {
            let mut items = Vec::new();
            for item in arr {
                items.push(from_raw(item.map_err(bson_error)?)?);
            }
            DocValue::Array(items)
        }
        RawBsonRef::Document(d) => from_raw_doc(d)?,
        RawBsonRef::Boolean(b) => DocValue::Bool(b),
        RawBsonRef::Null => DocValue::Null,
        RawBsonRef::RegularExpression(re) => DocValue::Regex {
            pattern: re.pattern.as_str().to_string(),
            options: re.options.as_str().to_string(),
        },
        RawBsonRef::JavaScriptCode(code) => {
            DocValue::Document(vec![("$code".into(), DocValue::String(code.into()))])
        }
        RawBsonRef::JavaScriptCodeWithScope(cws) => DocValue::Document(vec![
            ("$code".into(), DocValue::String(cws.code.into())),
            ("$scope".into(), from_raw_doc(cws.scope)?),
        ]),
        RawBsonRef::Int32(i) => DocValue::Int32(i),
        RawBsonRef::Int64(i) => DocValue::Int64(i),
        RawBsonRef::Timestamp(ts) => DocValue::Timestamp {
            t: ts.time,
            i: ts.increment,
        },
        RawBsonRef::Binary(b) => DocValue::Binary {
            subtype: u8::from(b.subtype),
            bytes: b.bytes.to_vec(),
        },
        RawBsonRef::ObjectId(id) => DocValue::ObjectId(id.bytes()),
        RawBsonRef::DateTime(dt) => DocValue::DateTime(dt.timestamp_millis()),
        RawBsonRef::Symbol(s) => {
            DocValue::Document(vec![("$symbol".into(), DocValue::String(s.to_string()))])
        }
        RawBsonRef::Decimal128(d) => DocValue::Decimal128(d.to_string()),
        RawBsonRef::Undefined => DocValue::Undefined,
        RawBsonRef::MaxKey => DocValue::MaxKey,
        RawBsonRef::MinKey => DocValue::MinKey,
        RawBsonRef::DbPointer(pointer) => {
            // The raw pointer's fields are crate-private: take its Extended JSON shape
            // (`{"$dbPointer": {"$ref": …, "$id": …}}`) through serde, which is not the
            // BSON serializer's special-cased DbPointer element.
            let json = serde_json::to_value(pointer).map_err(|e| {
                DriverError::new(
                    ErrorKind::Protocol,
                    format!("invalid BSON from the server: {e}"),
                )
            })?;
            from_json(&json)
        }
    })
}

/// A command reply (an owned `Document`) as a `DocValue`.
pub(crate) fn from_document(doc: &Document) -> Result<DocValue, DriverError> {
    let raw = RawDocumentBuf::try_from(doc).map_err(bson_error)?;
    from_raw_doc(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_db_pointer_keeps_its_namespace_and_id() {
        // A DBPointer element: type 0x0C, key "p", a string namespace and a 12-byte id.
        let namespace = "db.c";
        let oid = [7_u8; 12];
        let mut body = Vec::new();
        body.push(0x0C);
        body.extend_from_slice(b"p\0");
        body.extend_from_slice(&((namespace.len() as i32) + 1).to_le_bytes());
        body.extend_from_slice(namespace.as_bytes());
        body.push(0);
        body.extend_from_slice(&oid);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((body.len() + 5) as i32).to_le_bytes());
        bytes.extend_from_slice(&body);
        bytes.push(0);
        let raw = RawDocumentBuf::from_bytes(bytes).unwrap();

        let value = from_raw_doc(&raw).unwrap();
        assert_eq!(value.unmodelled(), Some("a DBPointer"));
        let DocValue::Document(fields) = &value else {
            panic!("{value:?}")
        };
        assert_eq!(fields[0].0, "p");
        let DocValue::Document(wrapper) = &fields[0].1 else {
            panic!("{:?}", fields[0].1)
        };
        assert_eq!(wrapper[0].0, "$dbPointer");
        let DocValue::Document(pointer) = &wrapper[0].1 else {
            panic!("{:?}", wrapper[0].1)
        };
        assert_eq!(
            pointer.iter().find(|(k, _)| k == "$ref").map(|(_, v)| v),
            Some(&DocValue::String(namespace.into()))
        );
        // serde renders the ObjectId as its Extended JSON wrapper.
        let DocValue::Document(id) = pointer
            .iter()
            .find(|(k, _)| k == "$id")
            .map(|(_, v)| v)
            .expect("$id")
        else {
            panic!("$id is not a document")
        };
        assert_eq!(
            id.iter().find(|(k, _)| k == "$oid").map(|(_, v)| v),
            Some(&DocValue::String("070707070707070707070707".into()))
        );
    }

    #[test]
    fn every_type_round_trips_through_raw_bson() {
        let doc = DocValue::Document(vec![
            ("a".into(), DocValue::Int32(1)),
            ("a".into(), DocValue::Int64(2)),
            ("d".into(), DocValue::Decimal128("1.10".into())),
            ("o".into(), DocValue::ObjectId([7; 12])),
            ("t".into(), DocValue::DateTime(-5)),
            (
                "b".into(),
                DocValue::Binary {
                    subtype: 0x85,
                    bytes: vec![1],
                },
            ),
            (
                "r".into(),
                DocValue::Regex {
                    pattern: "x".into(),
                    options: "i".into(),
                },
            ),
            ("ts".into(), DocValue::Timestamp { t: 1, i: 2 }),
            ("min".into(), DocValue::MinKey),
            ("max".into(), DocValue::MaxKey),
            ("u".into(), DocValue::Undefined),
            (
                "arr".into(),
                DocValue::Array(vec![DocValue::Null, DocValue::Bool(true)]),
            ),
        ]);
        let raw = to_raw_doc(&doc).unwrap();
        assert_eq!(
            from_raw_doc(&raw).unwrap(),
            doc,
            "duplicate keys and order kept"
        );
        assert!(to_raw_doc(&DocValue::Int32(1)).is_err());
        let nul = DocValue::Document(vec![("a\0b".into(), DocValue::Null)]);
        assert!(to_raw_doc(&nul).unwrap_err().message.contains("NUL"));
    }
}
