//! Query ids, result pages and their five-minute idle TTL.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use parking_lot::Mutex;
use serde_json::{Value, json};
use sqlr_core::{Cell, ColumnMeta, DisplayHint, DocValue, RespValue};
use sqlr_engine::{Engine, QueryId, QueryStatus};

use crate::error::{ToolError, ToolResult};

pub const RESULT_TTL: Duration = Duration::from_secs(300);
pub const MAX_PAGE_ROWS: usize = 1000;
pub const DEFAULT_PAGE_ROWS: usize = 200;
pub const MAX_PAGE_BYTES: usize = 256 * 1024;
/// A grid selection reaches an Agent with at most this many cells…
pub const MAX_SELECTION_CELLS: usize = 1000;
/// …and at most this many bytes of encoded values.
pub const MAX_SELECTION_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultKind {
    Tabular,
    Document,
    Reply,
}

struct Stored {
    engine: Option<QueryId>,
    kind: ResultKind,
    owner: String,
    last_access: Instant,
    reply: Option<Value>,
}

#[derive(Default)]
pub struct Results {
    next: AtomicU64,
    entries: Mutex<HashMap<String, Stored>>,
}

impl Results {
    pub fn insert(
        &self,
        owner: &str,
        kind: ResultKind,
        engine: QueryId,
        requested: Option<String>,
    ) -> String {
        let id = requested.unwrap_or_else(|| {
            let next = self.next.fetch_add(1, Ordering::Relaxed) + 1;
            format!("q{}-{}", std::process::id(), next)
        });
        self.entries.lock().insert(
            id.clone(),
            Stored {
                engine: Some(engine),
                kind,
                owner: owner.to_string(),
                last_access: Instant::now(),
                reply: None,
            },
        );
        id
    }

    pub fn insert_reply(&self, owner: &str, value: &RespValue) -> String {
        let next = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let id = format!("r{}-{}", std::process::id(), next);
        self.entries.lock().insert(
            id.clone(),
            Stored {
                engine: None,
                kind: ResultKind::Reply,
                owner: owner.to_string(),
                last_access: Instant::now(),
                reply: Some(resp_json(value)),
            },
        );
        id
    }

    pub fn reply(&self, id: &str) -> Option<Value> {
        let mut entries = self.entries.lock();
        let entry = entries.get_mut(id)?;
        entry.last_access = Instant::now();
        entry.reply.clone()
    }

    pub fn reply_for(&self, owner: &str, id: &str) -> Option<Value> {
        let mut entries = self.entries.lock();
        let entry = entries.get_mut(id).filter(|e| e.owner == owner)?;
        entry.last_access = Instant::now();
        entry.reply.clone()
    }

    pub fn get(&self, id: &str) -> Option<(QueryId, ResultKind)> {
        let mut entries = self.entries.lock();
        let entry = entries.get_mut(id)?;
        entry.last_access = Instant::now();
        Some((entry.engine?, entry.kind))
    }

    pub fn get_for(&self, owner: &str, id: &str) -> Option<(QueryId, ResultKind)> {
        let mut entries = self.entries.lock();
        let entry = entries.get_mut(id).filter(|e| e.owner == owner)?;
        entry.last_access = Instant::now();
        Some((entry.engine?, entry.kind))
    }

    pub fn touch(&self, id: &str) {
        if let Some(entry) = self.entries.lock().get_mut(id) {
            entry.last_access = Instant::now();
        }
    }

    pub fn touch_for(&self, owner: &str, id: &str) {
        if let Some(entry) = self.entries.lock().get_mut(id)
            && entry.owner == owner
        {
            entry.last_access = Instant::now();
        }
    }

    pub fn close(&self, id: &str) -> Option<QueryId> {
        self.entries.lock().remove(id).and_then(|e| e.engine)
    }

    /// Removes a result of any kind; the engine id is present except for replies.
    pub fn remove(&self, id: &str) -> Option<(Option<QueryId>, ResultKind)> {
        self.entries
            .lock()
            .remove(id)
            .map(|entry| (entry.engine, entry.kind))
    }

    pub fn remove_for(&self, owner: &str, id: &str) -> Option<(Option<QueryId>, ResultKind)> {
        let mut entries = self.entries.lock();
        let entry = entries.get(id)?;
        if entry.owner != owner {
            return None;
        }
        entries.remove(id).map(|entry| (entry.engine, entry.kind))
    }

    /// Closes every result of one MCP session.
    pub fn close_owner(&self, owner: &str) -> Vec<QueryId> {
        self.sweep_owner(owner)
            .into_iter()
            .map(|(q, _)| q)
            .collect()
    }

    /// Like [`Self::close_owner`] but keeps each result's kind.
    pub fn sweep_owner(&self, owner: &str) -> Vec<(QueryId, ResultKind)> {
        let mut removed = Vec::new();
        self.entries.lock().retain(|_, e| {
            if e.owner != owner {
                return true;
            }
            if let Some(q) = e.engine {
                removed.push((q, e.kind));
            }
            false
        });
        removed
    }

    /// Removes results idle for [`RESULT_TTL`] and returns what to close.
    pub fn sweep(&self) -> Vec<(QueryId, ResultKind)> {
        let now = Instant::now();
        let mut removed = Vec::new();
        self.entries.lock().retain(|_, e| {
            if now.duration_since(e.last_access) < RESULT_TTL {
                return true;
            }
            if let Some(q) = e.engine {
                removed.push((q, e.kind));
            }
            false
        });
        removed
    }

    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }
}

pub fn close_engine_result(engine: &Engine, id: QueryId, kind: ResultKind) {
    match kind {
        ResultKind::Document => engine.close_docs(id),
        _ => engine.close_result(id),
    }
}

pub fn default_rows(max_rows: Option<i64>) -> ToolResult<usize> {
    match max_rows {
        None => Ok(DEFAULT_PAGE_ROWS),
        Some(n) if (1..=MAX_PAGE_ROWS as i64).contains(&n) => Ok(n as usize),
        Some(_) => Err(ToolError::invalid(format!(
            "max_rows must be between 1 and {MAX_PAGE_ROWS}"
        ))),
    }
}

/// The `kind`/`hint` names the contract uses.
pub fn column_kind(column: &ColumnMeta) -> &'static str {
    match column.kind {
        sqlr_core::ColumnKind::Bool => "bool",
        sqlr_core::ColumnKind::Int => "int",
        sqlr_core::ColumnKind::Float => "float",
        sqlr_core::ColumnKind::Text => "text",
        sqlr_core::ColumnKind::Bytes => "bytes",
        sqlr_core::ColumnKind::Mixed => "mixed",
    }
}

pub fn hint_name(hint: DisplayHint) -> &'static str {
    match hint {
        DisplayHint::Plain => "plain",
        DisplayHint::Number => "number",
        DisplayHint::Decimal => "decimal",
        DisplayHint::Temporal => "temporal",
        DisplayHint::Interval => "interval",
        DisplayHint::Json => "json",
        DisplayHint::Xml => "xml",
        DisplayHint::Uuid => "uuid",
        DisplayHint::Array => "array",
        DisplayHint::Geometry => "geometry",
        DisplayHint::Binary => "binary",
    }
}

pub fn cell_json(cell: Cell<'_>, hint: DisplayHint) -> Value {
    match cell {
        Cell::Null => Value::Null,
        Cell::Bool(v) => Value::Bool(v),
        Cell::Int(v) => json!(v),
        Cell::Float(v) if v.is_finite() => json!(v),
        Cell::Float(v) if v.is_nan() => json!("NaN"),
        Cell::Float(v) if v.is_infinite() && v.is_sign_positive() => json!("Infinity"),
        Cell::Float(_) => json!("-Infinity"),
        Cell::Bytes(bytes) => json!(STANDARD.encode(bytes)),
        Cell::Text(text) => match hint {
            DisplayHint::Json => serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
            _ => json!(text),
        },
    }
}

pub fn column_json(column: &ColumnMeta) -> Value {
    json!({
        "name": column.name,
        "type_name": column.type_name,
        "kind": column_kind(column),
        "hint": hint_name(column.hint),
    })
}

pub fn message_json(level: sqlr_engine::MessageLevel, text: &str) -> Value {
    let level = match level {
        sqlr_engine::MessageLevel::Info => "info",
        sqlr_engine::MessageLevel::Notice => "notice",
        sqlr_engine::MessageLevel::Warning => "warning",
        sqlr_engine::MessageLevel::Error => "error",
    };
    json!({"level": level, "text": text})
}

pub struct TabularPage {
    pub columns: Vec<Value>,
    pub rows: Vec<Value>,
    pub affected: Option<u64>,
    pub messages: Vec<Value>,
    pub truncated: bool,
}

/// Copies a page of an engine rows result. The lock is released before returning.
pub async fn tabular_page(
    engine: &Engine,
    q: QueryId,
    offset: usize,
    limit: usize,
) -> ToolResult<(String, u128, Option<Value>, TabularPage)> {
    let Some(result) = engine.result(q) else {
        return Err(ToolError::not_found("The result expired or was closed"));
    };
    let (status, columns, total, elapsed_ms, affected, messages) = {
        let result = result.read();
        let status = match &result.status {
            QueryStatus::Running => "running",
            QueryStatus::Ready { .. } => "ready",
            QueryStatus::Cancelled => "cancelled",
            QueryStatus::Failed(_) => "failed",
        };
        (
            status,
            result
                .rows
                .columns()
                .iter()
                .map(column_json)
                .collect::<Vec<_>>(),
            result.rows.row_count(),
            result.elapsed.as_millis(),
            result.affected,
            result
                .messages
                .iter()
                .map(|m| message_json(m.level, &m.text))
                .collect::<Vec<_>>(),
        )
    };
    let error = {
        let result = result.read();
        match &result.status {
            QueryStatus::Failed(e) => Some(
                json!({"code": error_code(e.kind), "message": crate::error::redact(&e.message)}),
            ),
            _ => None,
        }
    };
    // Fetch more when the page asks beyond what is loaded and the stream may still have rows.
    if offset.saturating_add(limit) > total {
        let status = {
            let result = result.read();
            result.status.clone()
        };
        if status == (QueryStatus::Ready { more: true }) {
            engine.fetch_more(q);
            wait_ready(engine, q, Duration::from_secs(2)).await;
        }
    }
    let (rows, truncated) = {
        let result = result.read();
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        let mut truncated = false;
        let mut index = offset;
        while rows.len() < limit && index < result.rows.row_count() {
            let row: Vec<Value> = (0..result.rows.columns().len())
                .map(|col| {
                    result.rows.cell(index, col).map_or(Value::Null, |cell| {
                        cell_json(cell, result.rows.columns()[col].hint)
                    })
                })
                .collect();
            let encoded = serde_json::to_string(&row).map_or(0, |s| s.len());
            if bytes + encoded > MAX_PAGE_BYTES {
                truncated = true;
                break;
            }
            bytes += encoded;
            rows.push(Value::Array(row));
            index += 1;
        }
        if index < result.rows.row_count() {
            truncated = true;
        }
        if result.status == (QueryStatus::Ready { more: true }) {
            truncated = true;
        }
        (rows, truncated)
    };
    Ok((
        status.to_string(),
        elapsed_ms,
        error,
        TabularPage {
            columns,
            rows,
            affected,
            messages,
            truncated,
        },
    ))
}

fn error_code(kind: sqlr_core::ErrorKind) -> &'static str {
    match kind {
        sqlr_core::ErrorKind::Connection => "connection",
        sqlr_core::ErrorKind::Auth => "auth",
        sqlr_core::ErrorKind::Syntax => "syntax",
        sqlr_core::ErrorKind::Timeout => "timeout",
        sqlr_core::ErrorKind::Cancelled => "cancelled",
        sqlr_core::ErrorKind::Permission => "permission",
        sqlr_core::ErrorKind::Unsupported => "unsupported",
        sqlr_core::ErrorKind::Protocol => "protocol",
        sqlr_core::ErrorKind::Other => "error",
    }
}

pub async fn wait_ready(engine: &Engine, q: QueryId, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let status = engine.result(q).map(|r| r.read().status.clone());
        if status.as_ref() != Some(&QueryStatus::Running) {
            return;
        }
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub struct DocumentPage {
    pub documents: Vec<Value>,
    pub truncated: bool,
    pub total: usize,
}

pub async fn document_page(
    engine: &Engine,
    q: QueryId,
    offset: usize,
    limit: usize,
) -> ToolResult<(String, u128, Option<Value>, DocumentPage)> {
    let Some(initial) = engine.doc_result(q) else {
        return Err(ToolError::not_found("The result expired or was closed"));
    };
    let needs_more = {
        let result = initial.read();
        offset.saturating_add(limit) > result.len()
            && result.status == (QueryStatus::Ready { more: true })
    };
    if needs_more {
        engine.fetch_more_docs(q);
        wait_docs_ready(engine, q, Duration::from_secs(2)).await;
    }
    let Some(result) = engine.doc_result(q) else {
        return Err(ToolError::not_found("The result expired or was closed"));
    };
    let (status, elapsed_ms, messages, tabular, error) = {
        let result = result.read();
        let status = match &result.status {
            QueryStatus::Running => "running",
            QueryStatus::Ready { .. } => "ready",
            QueryStatus::Cancelled => "cancelled",
            QueryStatus::Failed(_) => "failed",
        };
        let error = match &result.status {
            QueryStatus::Failed(e) => Some(
                json!({"code": error_code(e.kind), "message": crate::error::redact(&e.message)}),
            ),
            _ => None,
        };
        (
            status,
            result.elapsed.as_millis(),
            result
                .messages
                .iter()
                .map(|m| message_json(m.level, &m.text))
                .collect::<Vec<_>>(),
            result.tabular,
            error,
        )
    };
    if tabular {
        // A tabular document result is an ordinary rows result with the same id.
        let (status, elapsed, error, page) = tabular_page(engine, q, offset, limit).await?;
        let mut all_messages = messages;
        all_messages.extend(page.messages);
        let total = page.rows.len();
        return Ok((
            status,
            elapsed,
            error,
            DocumentPage {
                documents: page.rows,
                truncated: page.truncated,
                total,
            },
        ));
    }
    let (documents, truncated, total) = {
        let result = result.read();
        let mut documents = Vec::new();
        let mut bytes = 0usize;
        let mut index = offset;
        let mut truncated = false;
        while documents.len() < limit && index < result.len() {
            let Some(doc) = result.doc(index) else {
                break;
            };
            let value = doc_json(&doc);
            let encoded = serde_json::to_string(&value).map_or(0, |s| s.len());
            if bytes + encoded > MAX_PAGE_BYTES {
                truncated = true;
                break;
            }
            bytes += encoded;
            documents.push(value);
            index += 1;
        }
        if index < result.len() || result.status == (QueryStatus::Ready { more: true }) {
            truncated = true;
        }
        (documents, truncated, result.len())
    };
    let _ = messages;
    Ok((
        status.to_string(),
        elapsed_ms,
        error,
        DocumentPage {
            documents,
            truncated,
            total,
        },
    ))
}

pub async fn wait_docs_ready(engine: &Engine, q: QueryId, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let status = engine.doc_result(q).map(|r| r.read().status.clone());
        if status.as_ref() != Some(&QueryStatus::Running) {
            return;
        }
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub fn doc_json(value: &DocValue) -> Value {
    match value {
        DocValue::Null | DocValue::Undefined | DocValue::MinKey | DocValue::MaxKey => Value::Null,
        DocValue::Bool(v) => Value::Bool(*v),
        DocValue::Int32(v) => json!(v),
        DocValue::Int64(v) => json!(v),
        DocValue::Double(v) if v.is_finite() => json!(v),
        DocValue::Double(_) => json!("NaN"),
        DocValue::Decimal128(v) => json!(v),
        DocValue::String(v) => json!(v),
        DocValue::ObjectId(bytes) => json!(hex(bytes)),
        DocValue::DateTime(ms) => json!(ms),
        DocValue::Binary { subtype, bytes } => {
            json!({"subtype": subtype, "base64": STANDARD.encode(bytes)})
        }
        DocValue::Regex { pattern, options } => json!({"pattern": pattern, "options": options}),
        DocValue::Timestamp { t, i } => json!({"t": t, "i": i}),
        DocValue::Array(items) => Value::Array(items.iter().map(doc_json).collect()),
        DocValue::Document(fields) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), doc_json(v)))
                .collect(),
        ),
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0xf)] as char);
    }
    out
}

pub fn resp_json(value: &RespValue) -> Value {
    match value {
        RespValue::Nil => Value::Null,
        RespValue::Int(v) => json!(v),
        RespValue::Simple(v) => json!(v),
        RespValue::Bulk(bytes) => match std::str::from_utf8(bytes) {
            Ok(text)
                if text
                    .chars()
                    .all(|c| !c.is_control() || c == '\n' || c == '\t') =>
            {
                json!(text)
            }
            _ => json!({"base64": STANDARD.encode(bytes)}),
        },
        RespValue::Array(items) => Value::Array(items.iter().map(resp_json).collect()),
        RespValue::Map(pairs) => Value::Array(
            pairs
                .iter()
                .map(|(k, v)| json!([resp_json(k), resp_json(v)]))
                .collect(),
        ),
        RespValue::Double(v) if v.is_finite() => json!(v),
        RespValue::Double(_) => json!("NaN"),
        RespValue::Bool(v) => json!(v),
        RespValue::Error(v) => json!({"error": v}),
        RespValue::Verbatim(v) => json!(v),
    }
}

/// The JSON envelope of one query outcome.
#[expect(
    clippy::too_many_arguments,
    reason = "the arguments are the independent fields of one JSON envelope (kind, status, query id, elapsed, truncation, error and the tabular, document or reply payload); a parameter struct would just repeat the field names at each of the few call sites"
)]
pub fn outcome_json(
    kind: &str,
    status: &str,
    query_id: Option<&str>,
    elapsed_ms: u128,
    truncated: bool,
    error: Option<Value>,
    tabular: Option<TabularPage>,
    document: Option<DocumentPage>,
    reply: Option<Value>,
) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("kind".into(), json!(kind));
    object.insert("status".into(), json!(status));
    if let Some(id) = query_id {
        object.insert("query_id".into(), json!(id));
    }
    object.insert("elapsed_ms".into(), json!(elapsed_ms));
    object.insert("truncated".into(), json!(truncated));
    if let Some(error) = error {
        object.insert("error".into(), error);
    }
    match kind {
        "tabular" => {
            if let Some(page) = tabular {
                object.insert(
                    "tabular".into(),
                    json!({
                        "columns": page.columns,
                        "rows": page.rows,
                        "affected": page.affected,
                        "messages": page.messages,
                    }),
                );
            }
        }
        "document" => {
            if let Some(page) = document {
                object.insert("document".into(), json!({"documents": page.documents}));
            }
        }
        "reply" => {
            object.insert(
                "reply".into(),
                json!({"value": reply.unwrap_or(Value::Null)}),
            );
        }
        _ => {}
    }
    Value::Object(object)
}

pub type SharedResults = Arc<Results>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_values_keep_extended_types_and_nested_structure() {
        let cases = [
            (DocValue::Undefined, Value::Null),
            (DocValue::MinKey, Value::Null),
            (DocValue::MaxKey, Value::Null),
            (DocValue::Null, Value::Null),
            (DocValue::Bool(true), json!(true)),
            (DocValue::Int32(-4), json!(-4)),
            (DocValue::Int64(i64::MAX), json!(9223372036854775807i64)),
            (DocValue::Double(1.5), json!(1.5)),
            (DocValue::Double(f64::NAN), json!("NaN")),
            (DocValue::Decimal128("1.100".into()), json!("1.100")),
            (DocValue::String("hello".into()), json!("hello")),
            (
                DocValue::ObjectId([0xab; 12]),
                json!("abababababababababababab"),
            ),
            (DocValue::DateTime(1234), json!(1234)),
            (
                DocValue::Binary {
                    subtype: 4,
                    bytes: vec![0, 255],
                },
                json!({"subtype":4,"base64":"AP8="}),
            ),
            (
                DocValue::Regex {
                    pattern: "^a".into(),
                    options: "i".into(),
                },
                json!({"pattern":"^a","options":"i"}),
            ),
            (DocValue::Timestamp { t: 42, i: 7 }, json!({"t":42,"i":7})),
            (
                DocValue::Document(vec![(
                    "nested".into(),
                    DocValue::Array(vec![DocValue::Null, DocValue::Int32(3)]),
                )]),
                json!({"nested":[null,3]}),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(doc_json(&input), expected, "{input:?}");
        }
    }

    #[test]
    fn redis_values_preserve_binary_data_and_non_string_map_keys() {
        let cases = [
            (RespValue::Nil, Value::Null),
            (RespValue::Int(42), json!(42)),
            (RespValue::Simple("OK".into()), json!("OK")),
            (RespValue::Bulk(b"hello\n\t".to_vec()), json!("hello\n\t")),
            (RespValue::Bulk(vec![0, 255]), json!({"base64":"AP8="})),
            (RespValue::Bulk(vec![0]), json!({"base64":"AA=="})),
            (
                RespValue::Array(vec![RespValue::Int(1), RespValue::Nil]),
                json!([1, null]),
            ),
            (
                RespValue::Map(vec![(RespValue::Int(1), RespValue::Bool(true))]),
                json!([[1, true]]),
            ),
            (RespValue::Double(1.5), json!(1.5)),
            (RespValue::Double(f64::INFINITY), json!("NaN")),
            (RespValue::Bool(false), json!(false)),
            (
                RespValue::Error("WRONGTYPE".into()),
                json!({"error":"WRONGTYPE"}),
            ),
            (RespValue::Verbatim("text".into()), json!("text")),
        ];
        for (input, expected) in cases {
            assert_eq!(resp_json(&input), expected, "{input:?}");
        }
    }

    #[test]
    fn sweeping_expires_only_idle_results_and_keeps_their_kind() {
        let results = Results::default();
        let old = results.insert("s1", ResultKind::Document, QueryId(1), Some("old".into()));
        let touched = results.insert("s1", ResultKind::Tabular, QueryId(2), None);
        let reply = results.insert_reply("s1", &RespValue::Int(42));
        for id in [&old, &touched, &reply] {
            results.entries.lock().get_mut(id).unwrap().last_access = Instant::now() - RESULT_TTL;
        }
        results.touch(&touched);
        assert_eq!(results.sweep(), vec![(QueryId(1), ResultKind::Document)]);
        assert_eq!(results.get(&old), None);
        assert_eq!(results.reply(&reply), None);
        assert_eq!(
            results.get(&touched),
            Some((QueryId(2), ResultKind::Tabular))
        );
        assert_eq!(results.len(), 1);
        assert!(!results.is_empty());
        assert_eq!(results.sweep_owner("other"), vec![]);
        assert_eq!(
            results.sweep_owner("s1"),
            vec![(QueryId(2), ResultKind::Tabular)]
        );
        assert!(results.is_empty());
    }

    #[test]
    fn value_mapping_preserves_precision_and_specials() {
        assert_eq!(cell_json(Cell::Null, DisplayHint::Plain), Value::Null);
        assert_eq!(cell_json(Cell::Bool(true), DisplayHint::Plain), json!(true));
        assert_eq!(cell_json(Cell::Int(3), DisplayHint::Plain), json!(3));
        assert_eq!(cell_json(Cell::Float(1.5), DisplayHint::Plain), json!(1.5));
        assert_eq!(
            cell_json(Cell::Float(f64::NAN), DisplayHint::Plain),
            json!("NaN")
        );
        assert_eq!(
            cell_json(Cell::Float(f64::INFINITY), DisplayHint::Plain),
            json!("Infinity")
        );
        assert_eq!(
            cell_json(Cell::Float(f64::NEG_INFINITY), DisplayHint::Plain),
            json!("-Infinity")
        );
        assert_eq!(
            cell_json(Cell::Text("1.10"), DisplayHint::Decimal),
            json!("1.10")
        );
        assert_eq!(
            cell_json(Cell::Text("{\"a\":1}"), DisplayHint::Json),
            json!({"a": 1})
        );
        assert_eq!(
            cell_json(Cell::Text("not json"), DisplayHint::Json),
            json!("not json")
        );
        assert_eq!(
            cell_json(Cell::Bytes(&[0, 255]), DisplayHint::Binary),
            json!("AP8=")
        );
    }

    #[test]
    fn result_store_expires_and_closes_by_owner() {
        let results = Results::default();
        let engine = QueryId(1);
        let id = results.insert("s1", ResultKind::Tabular, engine, None);
        assert!(results.get(&id).is_some());
        assert_eq!(results.close_owner("s1"), vec![engine]);
        assert!(results.get(&id).is_none());
        let id = results.insert("s2", ResultKind::Tabular, engine, None);
        assert_eq!(results.close(&id), Some(engine));
        assert_eq!(results.close(&id), None);
    }
}
