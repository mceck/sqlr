//! Document values (MongoDB's BSON types, Elasticsearch's JSON) and Extended JSON v2.
//!
//! Documents keep their key order and duplicate keys. Two text forms exist:
//! - **relaxed** ([`DocValue::to_relaxed_json`]): readable relaxed Extended JSON, written so
//!   that [`DocValue::from_relaxed_json`] reads back every type exactly (an `Int64` that
//!   would read back as `Int32` keeps its `$numberLong` wrapper, a whole `Double` keeps its
//!   `.0`). This is the text the JSON view shows and edits.
//! - **canonical** ([`DocValue::to_canonical_ejson`]): canonical Extended JSON as a
//!   `serde_json::Value` (key order kept, duplicate keys collapse to the last one).

use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum DocValue {
    Null,
    Bool(bool),
    Int32(i32),
    Int64(i64),
    Double(f64),
    /// The decimal's canonical string (`"1.10"`), so no precision is lost.
    Decimal128(String),
    String(String),
    ObjectId([u8; 12]),
    /// Milliseconds since the Unix epoch, UTC.
    DateTime(i64),
    Binary {
        subtype: u8,
        bytes: Vec<u8>,
    },
    Regex {
        pattern: String,
        options: String,
    },
    Timestamp {
        t: u32,
        i: u32,
    },
    MinKey,
    MaxKey,
    Array(Vec<DocValue>),
    Document(Vec<(String, DocValue)>),
    Undefined,
}

/// Nesting deeper than this is refused when reading text (BSON allows 100 levels).
pub const MAX_DOC_DEPTH: usize = 512;

impl DocValue {
    /// The shell's name for the type (`"objectId"`, `"long"`, …), as `$type` spells it.
    pub fn type_name(&self) -> &'static str {
        match self {
            DocValue::Null => "null",
            DocValue::Bool(_) => "bool",
            DocValue::Int32(_) => "int",
            DocValue::Int64(_) => "long",
            DocValue::Double(_) => "double",
            DocValue::Decimal128(_) => "decimal",
            DocValue::String(_) => "string",
            DocValue::ObjectId(_) => "objectId",
            DocValue::DateTime(_) => "date",
            DocValue::Binary { .. } => "binData",
            DocValue::Regex { .. } => "regex",
            DocValue::Timestamp { .. } => "timestamp",
            DocValue::MinKey => "minKey",
            DocValue::MaxKey => "maxKey",
            DocValue::Array(_) => "array",
            DocValue::Document(_) => "object",
            DocValue::Undefined => "undefined",
        }
    }

    /// The value of `key` in a document (the first one when the key repeats).
    pub fn get(&self, key: &str) -> Option<&DocValue> {
        match self {
            DocValue::Document(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Relaxed Extended JSON on one line (see the module docs).
    pub fn to_relaxed_json(&self) -> String {
        let mut out = String::new();
        write_relaxed(self, &mut out, None, 0);
        out
    }

    /// Relaxed Extended JSON, documents and arrays indented by two spaces; type wrappers
    /// (`{"$oid": …}`) stay on one line.
    pub fn to_relaxed_json_pretty(&self) -> String {
        let mut out = String::new();
        write_relaxed(self, &mut out, Some(2), 0);
        out
    }

    /// Reads relaxed or canonical Extended JSON text, also accepting the mongo shell's
    /// literals (`ObjectId("…")`, unquoted keys, comments, …). Duplicate keys are kept.
    pub fn from_relaxed_json(text: &str) -> Result<DocValue, crate::ShellError> {
        crate::mongo_shell::parse_shell_value(text)
    }

    /// What this value holds that [`DocValue`] cannot write back without loss: drivers show
    /// such BSON types (JavaScript code, symbols, DBPointers) as a document whose first key
    /// is `$code`, `$symbol` or `$dbPointer`. `None` when every part round-trips.
    pub fn unmodelled(&self) -> Option<&'static str> {
        match self {
            DocValue::Document(fields) => {
                match fields.first().map(|(k, _)| k.as_str()) {
                    Some("$code") => return Some("JavaScript code"),
                    Some("$symbol") => return Some("a symbol"),
                    Some("$dbPointer") => return Some("a DBPointer"),
                    _ => {}
                }
                fields.iter().find_map(|(_, v)| v.unmodelled())
            }
            DocValue::Array(items) => items.iter().find_map(DocValue::unmodelled),
            _ => None,
        }
    }

    /// Plain JSON, as Elasticsearch stores it: types JSON lacks become strings (dates as
    /// ISO 8601, ObjectIds as hex, binary as base64).
    pub fn to_plain_json(&self) -> serde_json::Value {
        use serde_json::{Value, json};
        match self {
            DocValue::Null | DocValue::Undefined | DocValue::MinKey | DocValue::MaxKey => {
                Value::Null
            }
            DocValue::Bool(b) => Value::Bool(*b),
            DocValue::Int32(i) => json!(i),
            DocValue::Int64(i) => json!(i),
            DocValue::Double(d) => serde_json::Number::from_f64(*d)
                .map_or_else(|| Value::String(d.to_string()), Value::Number),
            DocValue::Decimal128(s) | DocValue::String(s) => Value::String(s.clone()),
            DocValue::ObjectId(b) => Value::String(hex(b)),
            DocValue::DateTime(ms) => Value::String(format_iso_date(*ms)),
            DocValue::Binary { bytes, .. } => Value::String(base64_encode(bytes)),
            DocValue::Regex { pattern, options } => Value::String(format!("/{pattern}/{options}")),
            DocValue::Timestamp { t, .. } => json!(t),
            DocValue::Array(items) => {
                Value::Array(items.iter().map(DocValue::to_plain_json).collect())
            }
            DocValue::Document(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_plain_json()))
                    .collect(),
            ),
        }
    }

    /// Canonical Extended JSON v2.
    pub fn to_canonical_ejson(&self) -> serde_json::Value {
        use serde_json::{Value, json};
        match self {
            DocValue::Null => Value::Null,
            DocValue::Bool(b) => Value::Bool(*b),
            DocValue::Int32(v) => json!({"$numberInt": v.to_string()}),
            DocValue::Int64(v) => json!({"$numberLong": v.to_string()}),
            DocValue::Double(v) => json!({"$numberDouble": double_text(*v)}),
            DocValue::Decimal128(s) => json!({"$numberDecimal": s}),
            DocValue::String(s) => Value::String(s.clone()),
            DocValue::ObjectId(b) => json!({"$oid": hex(b)}),
            DocValue::DateTime(ms) => json!({"$date": {"$numberLong": ms.to_string()}}),
            DocValue::Binary { subtype, bytes } => {
                json!({"$binary": {"base64": base64_encode(bytes), "subType": format!("{subtype:02x}")}})
            }
            DocValue::Regex { pattern, options } => {
                json!({"$regularExpression": {"pattern": pattern, "options": options}})
            }
            DocValue::Timestamp { t, i } => json!({"$timestamp": {"t": t, "i": i}}),
            DocValue::MinKey => json!({"$minKey": 1}),
            DocValue::MaxKey => json!({"$maxKey": 1}),
            DocValue::Undefined => json!({"$undefined": true}),
            DocValue::Array(items) => {
                Value::Array(items.iter().map(DocValue::to_canonical_ejson).collect())
            }
            DocValue::Document(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_canonical_ejson()))
                    .collect(),
            ),
        }
    }

    /// Reads canonical or relaxed Extended JSON. Plain JSON numbers become `Int32` when
    /// they fit, else `Int64`, else `Double`.
    pub fn from_ejson(v: &serde_json::Value) -> Result<DocValue, String> {
        use serde_json::Value;
        Ok(match v {
            Value::Null => DocValue::Null,
            Value::Bool(b) => DocValue::Bool(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    int_value(i)
                } else {
                    DocValue::Double(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            Value::String(s) => DocValue::String(s.clone()),
            Value::Array(items) => DocValue::Array(
                items
                    .iter()
                    .map(DocValue::from_ejson)
                    .collect::<Result<_, _>>()?,
            ),
            Value::Object(map) => {
                let fields = map
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), DocValue::from_ejson(v)?)))
                    .collect::<Result<Vec<_>, String>>()?;
                decode_wrapper(fields)?
            }
        })
    }
}

/// `i` as `Int32` when it fits, else `Int64`.
pub(crate) fn int_value(i: i64) -> DocValue {
    match i32::try_from(i) {
        Ok(v) => DocValue::Int32(v),
        Err(_) => DocValue::Int64(i),
    }
}

/// A document of exactly one Extended JSON wrapper (`{"$oid": …}`, …) as the value it
/// stands for; any other document unchanged. A wrapper key with a malformed value is an
/// error. `$regex`/`$options` is a query operator and stays a document.
pub(crate) fn decode_wrapper(fields: Vec<(String, DocValue)>) -> Result<DocValue, String> {
    let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
    let first = fields.first().map(|(_, v)| v);
    let text = |v: Option<&DocValue>, what: &str| match v {
        Some(DocValue::String(s)) => Ok(s.clone()),
        _ => Err(format!("{what} needs a string")),
    };
    let value = match keys.as_slice() {
        ["$oid"] => DocValue::ObjectId(parse_object_id(&text(first, "$oid")?)?),
        ["$numberInt"] => {
            let s = text(first, "$numberInt")?;
            DocValue::Int32(
                s.parse()
                    .map_err(|_| format!("invalid $numberInt \"{s}\""))?,
            )
        }
        ["$numberLong"] => {
            let s = text(first, "$numberLong")?;
            DocValue::Int64(
                s.parse()
                    .map_err(|_| format!("invalid $numberLong \"{s}\""))?,
            )
        }
        ["$numberDouble"] => DocValue::Double(parse_double_text(&text(first, "$numberDouble")?)?),
        ["$numberDecimal"] => {
            let s = text(first, "$numberDecimal")?;
            check_decimal(&s)?;
            DocValue::Decimal128(s)
        }
        ["$date"] => DocValue::DateTime(match first {
            Some(DocValue::String(s)) => parse_iso_date(s)?,
            Some(DocValue::Int32(v)) => i64::from(*v),
            Some(DocValue::Int64(v)) => *v,
            Some(DocValue::Double(v)) if v.fract() == 0.0 => *v as i64,
            // Inner wrappers decode first, so `{"$numberLong": "…"}` arrives as `Int64`.
            _ => return Err("invalid $date".into()),
        }),
        ["$binary"] => match first {
            Some(d @ DocValue::Document(_)) if d_len(d) == 2 => {
                let b64 = text(d.get("base64"), "$binary.base64")?;
                let sub = text(d.get("subType"), "$binary.subType")?;
                binary(&b64, &sub)?
            }
            _ => return Err("invalid $binary".into()),
        },
        ["$binary", "$type"] => {
            let b64 = text(first, "$binary")?;
            let sub = text(fields.get(1).map(|(_, v)| v), "$type")?;
            binary(&b64, &sub)?
        }
        ["$uuid"] => DocValue::Binary {
            subtype: 4,
            bytes: parse_uuid(&text(first, "$uuid")?)?,
        },
        ["$regularExpression"] => match first {
            Some(d @ DocValue::Document(_)) if d_len(d) == 2 => DocValue::Regex {
                pattern: text(d.get("pattern"), "$regularExpression.pattern")?,
                options: text(d.get("options"), "$regularExpression.options")?,
            },
            _ => return Err("invalid $regularExpression".into()),
        },
        ["$timestamp"] => match first {
            Some(d @ DocValue::Document(_)) if d_len(d) == 2 => DocValue::Timestamp {
                t: u32_field(d.get("t")).ok_or("invalid $timestamp.t")?,
                i: u32_field(d.get("i")).ok_or("invalid $timestamp.i")?,
            },
            _ => return Err("invalid $timestamp".into()),
        },
        ["$minKey"] => DocValue::MinKey,
        ["$maxKey"] => DocValue::MaxKey,
        ["$undefined"] => DocValue::Undefined,
        // A symbol is not modelled: it stays a `{"$symbol": …}` document, which
        // `unmodelled` names, so it is never written back as a string.
        _ => return Ok(DocValue::Document(fields)),
    };
    Ok(value)
}

fn d_len(d: &DocValue) -> usize {
    match d {
        DocValue::Document(f) => f.len(),
        _ => 0,
    }
}

fn u32_field(v: Option<&DocValue>) -> Option<u32> {
    match v? {
        DocValue::Int32(i) => u32::try_from(*i).ok(),
        DocValue::Int64(i) => u32::try_from(*i).ok(),
        DocValue::Double(d) if d.fract() == 0.0 && *d >= 0.0 && *d <= f64::from(u32::MAX) => {
            Some(*d as u32)
        }
        _ => None,
    }
}

fn binary(b64: &str, sub: &str) -> Result<DocValue, String> {
    Ok(DocValue::Binary {
        subtype: u8::from_str_radix(sub, 16).map_err(|_| format!("invalid subType \"{sub}\""))?,
        bytes: base64_decode(b64)?,
    })
}

/// A decimal string as MongoDB accepts it: digits with an optional sign, point and
/// exponent, or `Infinity`/`NaN`.
pub(crate) fn check_decimal(s: &str) -> Result<(), String> {
    let t = s.strip_prefix(['-', '+']).unwrap_or(s);
    if matches!(t, "Infinity" | "Inf" | "NaN") {
        return Ok(());
    }
    let (mantissa, exp) = match t.find(['e', 'E']) {
        Some(i) => (&t[..i], Some(&t[i + 1..])),
        None => (t, None),
    };
    let digits = mantissa.chars().filter(char::is_ascii_digit).count();
    let ok_mantissa = digits > 0
        && mantissa.chars().all(|c| c.is_ascii_digit() || c == '.')
        && mantissa.matches('.').count() <= 1;
    let ok_exp = exp.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && e.chars().all(|c| c.is_ascii_digit())
    });
    if ok_mantissa && ok_exp && digits <= 34 {
        Ok(())
    } else {
        Err(format!("invalid decimal \"{s}\""))
    }
}

/// `f64` text that reads back exactly: `1.0`, `0.1`, `1e300`, `NaN`, `Infinity`.
pub(crate) fn double_text(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else {
        // Debug keeps a `.0` on whole numbers and switches to exponents for very large or
        // small magnitudes; both forms are valid JSON numbers.
        let s = format!("{v:?}");
        if s.contains(['.', 'e', 'E']) {
            s
        } else {
            format!("{s}.0")
        }
    }
}

fn parse_double_text(s: &str) -> Result<f64, String> {
    match s {
        "NaN" => Ok(f64::NAN),
        "Infinity" | "+Infinity" => Ok(f64::INFINITY),
        "-Infinity" => Ok(f64::NEG_INFINITY),
        _ => s
            .parse()
            .map_err(|_| format!("invalid $numberDouble \"{s}\"")),
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The relaxed form of non-container values (wrappers on one line).
fn write_scalar(v: &DocValue, out: &mut String) {
    let mut wrap = |key: &str, body: &str| {
        let _ = write!(out, "{{\"{key}\": {body}}}");
    };
    match v {
        DocValue::Null => out.push_str("null"),
        DocValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        DocValue::Int32(i) => {
            let _ = write!(out, "{i}");
        }
        DocValue::Int64(i) => {
            if i32::try_from(*i).is_ok() {
                wrap("$numberLong", &format!("\"{i}\""));
            } else {
                let _ = write!(out, "{i}");
            }
        }
        DocValue::Double(d) if d.is_finite() => out.push_str(&double_text(*d)),
        DocValue::Double(d) => wrap("$numberDouble", &format!("\"{}\"", double_text(*d))),
        DocValue::Decimal128(s) => {
            let mut body = String::new();
            write_json_string(s, &mut body);
            wrap("$numberDecimal", &body);
        }
        DocValue::String(s) => write_json_string(s, out),
        DocValue::ObjectId(b) => wrap("$oid", &format!("\"{}\"", hex(b))),
        DocValue::DateTime(ms) => {
            if (0..YEAR_10000_MS).contains(ms) {
                wrap("$date", &format!("\"{}\"", format_iso_date(*ms)));
            } else {
                wrap("$date", &format!("{{\"$numberLong\": \"{ms}\"}}"));
            }
        }
        DocValue::Binary { subtype, bytes } => wrap(
            "$binary",
            &format!(
                "{{\"base64\": \"{}\", \"subType\": \"{subtype:02x}\"}}",
                base64_encode(bytes)
            ),
        ),
        DocValue::Regex { pattern, options } => {
            let mut p = String::new();
            write_json_string(pattern, &mut p);
            let mut o = String::new();
            write_json_string(options, &mut o);
            wrap(
                "$regularExpression",
                &format!("{{\"pattern\": {p}, \"options\": {o}}}"),
            );
        }
        DocValue::Timestamp { t, i } => {
            wrap("$timestamp", &format!("{{\"t\": {t}, \"i\": {i}}}"));
        }
        DocValue::MinKey => wrap("$minKey", "1"),
        DocValue::MaxKey => wrap("$maxKey", "1"),
        DocValue::Undefined => wrap("$undefined", "true"),
        DocValue::Array(_) | DocValue::Document(_) => unreachable!("containers"),
    }
}

fn write_relaxed(v: &DocValue, out: &mut String, indent: Option<usize>, level: usize) {
    let newline = |out: &mut String, level: usize| {
        if let Some(n) = indent {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', n * level));
        }
    };
    match v {
        DocValue::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if indent.is_none() {
                        out.push(' ');
                    }
                }
                newline(out, level + 1);
                write_relaxed(item, out, indent, level + 1);
            }
            newline(out, level);
            out.push(']');
        }
        DocValue::Document(fields) => {
            if fields.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, item)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if indent.is_none() {
                        out.push(' ');
                    }
                }
                newline(out, level + 1);
                write_json_string(k, out);
                out.push_str(": ");
                write_relaxed(item, out, indent, level + 1);
            }
            newline(out, level);
            out.push('}');
        }
        scalar => write_scalar(scalar, out),
    }
}

// ---- ObjectId, UUID, hex and base64 -------------------------------------------------------

pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_digits(s: &str, n: usize, what: &str) -> Result<Vec<u8>, String> {
    let bad = || format!("invalid {what} \"{s}\"");
    if s.len() != n * 2 || !s.is_ascii() {
        return Err(bad());
    }
    (0..n)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| bad()))
        .collect()
}

/// 24 hex digits, either case.
pub fn parse_object_id(s: &str) -> Result<[u8; 12], String> {
    let v = hex_digits(s, 12, "ObjectId")?;
    let mut out = [0u8; 12];
    out.copy_from_slice(&v);
    Ok(out)
}

/// `00112233-4455-6677-8899-aabbccddeeff` (dashes optional).
pub fn parse_uuid(s: &str) -> Result<Vec<u8>, String> {
    let plain: String = s.chars().filter(|&c| c != '-').collect();
    hex_digits(&plain, 16, "UUID").map_err(|_| format!("invalid UUID \"{s}\""))
}

/// A fresh ObjectId: seconds since the epoch, then a per-process random value and a counter.
pub fn new_object_id() -> [u8; 12] {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU32, Ordering};
    static PROCESS: OnceLock<[u8; 5]> = OnceLock::new();
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let process = PROCESS.get_or_init(|| {
        // RandomState is seeded from the OS; hash the clock and the pid for 5 bytes.
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(now.as_nanos());
        h.write_u32(std::process::id());
        let v = h.finish().to_be_bytes();
        [v[0], v[1], v[2], v[3], v[4]]
    });
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut id = [0u8; 12];
    id[..4].copy_from_slice(&(now.as_secs() as u32).to_be_bytes());
    id[4..9].copy_from_slice(process);
    id[9..].copy_from_slice(&count.to_be_bytes()[1..]);
    id
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let bad = || format!("invalid base64 \"{s}\"");
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return Err(bad()),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if bits >= 6 {
        return Err(bad());
    }
    Ok(out)
}

// ---- ISO 8601 dates ------------------------------------------------------------------------

/// 10000-01-01T00:00:00Z in milliseconds: relaxed `$date` strings stop before it.
const YEAR_10000_MS: i64 = 253_402_300_800_000;

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// `2024-02-29T12:34:56.789Z`; the fraction only when the milliseconds are not zero.
pub fn format_iso_date(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let (h, min, s, milli) = (
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000,
    );
    let mut out = format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}");
    if milli != 0 {
        let _ = write!(out, ".{milli:03}");
    }
    out.push('Z');
    out
}

/// Parses `YYYY-MM-DD[(T| )HH:MM[:SS[.fff…]]][Z|±HH[:]MM]`; no zone means UTC.
pub fn parse_iso_date(s: &str) -> Result<i64, String> {
    let bad = || format!("invalid ISO 8601 date \"{s}\" (expected e.g. \"2024-01-31T12:00:00Z\")");
    let b = s.trim().as_bytes();
    let num = |from: usize, len: usize| -> Result<i64, String> {
        let part = b.get(from..from + len).ok_or_else(bad)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return Err(bad());
        }
        Ok(std::str::from_utf8(part).unwrap().parse().unwrap())
    };
    let year = num(0, 4)?;
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return Err(bad());
    }
    let (month, day) = (num(5, 2)? as u32, num(8, 2)? as u32);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&month) || day == 0 || day > month_days[month as usize - 1] {
        return Err(bad());
    }
    let mut ms = days_from_civil(year, month, day) * 86_400_000;
    let mut i = 10;
    if i < b.len() && (b[i] == b'T' || b[i] == b't' || b[i] == b' ') {
        let (h, mi) = (num(i + 1, 2)?, num(i + 4, 2)?);
        if b.get(i + 3) != Some(&b':') || h > 23 || mi > 59 {
            return Err(bad());
        }
        ms += h * 3_600_000 + mi * 60_000;
        i += 6;
        if b.get(i) == Some(&b':') {
            let sec = num(i + 1, 2)?;
            if sec > 60 {
                return Err(bad());
            }
            ms += sec * 1000;
            i += 3;
            if b.get(i) == Some(&b'.') {
                i += 1;
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i == start {
                    return Err(bad());
                }
                let frac = &s.trim()[start..i];
                let milli: String = frac.chars().chain("000".chars()).take(3).collect();
                ms += milli.parse::<i64>().unwrap();
            }
        }
    }
    match b.get(i) {
        None => {}
        Some(b'Z' | b'z') if i + 1 == b.len() => {}
        Some(&sign @ (b'+' | b'-')) => {
            let h = num(i + 1, 2)?;
            let m = if b.get(i + 3) == Some(&b':') {
                num(i + 4, 2)?
            } else {
                num(i + 3, 2)?
            };
            let end = i + if b.get(i + 3) == Some(&b':') { 6 } else { 5 };
            if end != b.len() || h > 23 || m > 59 {
                return Err(bad());
            }
            let offset = (h * 60 + m) * 60_000;
            ms -= if sign == b'+' { offset } else { -offset };
        }
        _ => return Err(bad()),
    }
    Ok(ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmodelled_types_are_found_at_any_depth() {
        let code = DocValue::Document(vec![("$code".into(), DocValue::String("f()".into()))]);
        let deep = DocValue::Document(vec![
            ("_id".into(), DocValue::Int32(1)),
            ("a".into(), DocValue::Array(vec![DocValue::Null, code])),
        ]);
        assert_eq!(deep.unmodelled(), Some("JavaScript code"));
        let symbol = DocValue::Document(vec![("$symbol".into(), DocValue::String("s".into()))]);
        assert_eq!(symbol.unmodelled(), Some("a symbol"));
        // `$code` after another key is an ordinary field name.
        let plain = DocValue::Document(vec![
            ("a".into(), DocValue::Int32(1)),
            ("$code".into(), DocValue::Int32(2)),
        ]);
        assert_eq!(plain.unmodelled(), None);
    }

    #[test]
    fn plain_json_has_no_wrappers() {
        let d = DocValue::Document(vec![
            ("n".into(), DocValue::Int64(5)),
            ("o".into(), DocValue::ObjectId([1; 12])),
            ("t".into(), DocValue::DateTime(0)),
        ]);
        assert_eq!(
            d.to_plain_json(),
            serde_json::json!({"n": 5, "o": "010101010101010101010101", "t": "1970-01-01T00:00:00Z"})
        );
    }

    #[test]
    fn iso_dates_round_trip() {
        for ms in [
            0,
            1,
            -1,
            951_782_400_000,
            1_709_210_096_789,
            -62_135_596_800_000,
            YEAR_10000_MS - 1,
        ] {
            assert_eq!(parse_iso_date(&format_iso_date(ms)).unwrap(), ms, "{ms}");
        }
        assert_eq!(format_iso_date(-1), "1969-12-31T23:59:59.999Z");
        assert_eq!(parse_iso_date("2024-02-29").unwrap(), 1_709_164_800_000);
        assert_eq!(
            parse_iso_date("2020-01-01T01:00:00+01:00").unwrap(),
            1_577_836_800_000
        );
        assert_eq!(
            parse_iso_date("2020-01-01 00:00:00.5").unwrap(),
            1_577_836_800_500
        );
        for bad in [
            "2023-02-29",
            "01/01/2012",
            "2020-01-01T25:00",
            "2020-01-01Zx",
            "",
        ] {
            assert!(parse_iso_date(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn base64_and_ids() {
        for bytes in [&b""[..], b"a", b"ab", b"abc", b"abcd", &[0, 255, 128]] {
            assert_eq!(base64_decode(&base64_encode(bytes)).unwrap(), bytes);
        }
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert!(base64_decode("A").is_err());
        let a = new_object_id();
        let b = new_object_id();
        assert_ne!(a, b);
        assert_eq!(parse_object_id(&hex(&a)).unwrap(), a);
        assert!(check_decimal("1.10").is_ok() && check_decimal("-1E+5").is_ok());
        assert!(check_decimal("1.2.3").is_err() && check_decimal("abc").is_err());
        assert_eq!(double_text(1.0), "1.0");
        assert_eq!(double_text(1e300), "1e300");
        assert_eq!(double_text(-0.0), "-0.0");
    }
}
