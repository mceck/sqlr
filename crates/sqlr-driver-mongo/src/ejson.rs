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

fn bson_error(e: bson::error::Error) -> DriverError {
    DriverError::new(
        ErrorKind::Protocol,
        format!("invalid BSON from the server: {e}"),
    )
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
        RawBsonRef::DbPointer(_) => DocValue::Document(vec![("$dbPointer".into(), DocValue::Null)]),
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
