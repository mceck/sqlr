//! Redis keys as a transfer source: one row per key matching a pattern (`SCAN`, never
//! `KEYS`), with its name, type, TTL in milliseconds and value as JSON.
//!
//! A value is read a page at a time and written as compact JSON: a string as a JSON string,
//! a hash as an object, a list or set as an array, a sorted set as `[member, score]` pairs,
//! a stream as `{id, fields}` entries. Bytes that are not UTF-8 become `{"base64": …}`. A
//! value larger than [`MAX_VALUE_BYTES`] stops the transfer with the key's name: it cannot
//! fit one cell.

use std::sync::Arc;

use serde_json::{Map, Value as Json, json};
use sqlr_core::{
    ColumnKind, ColumnMeta, DisplayHint, DriverError, ErrorKind, KeyType, KeyValueConn, KvCursor,
    KvPage, SCAN_COUNT, VALUE_PAGE, Value, base64_encode, display_key,
};
use tokio::sync::mpsc;

use super::pipeline::Records;
use crate::engine::{Shared, not_connected};
use crate::session::SessionKey;

/// The largest value one row takes.
pub(crate) const MAX_VALUE_BYTES: usize = 64 << 20;
/// Keys per batch sent to the sink.
const KEYS_PER_BATCH: usize = 200;

fn bytes_json(b: &[u8]) -> Json {
    match std::str::from_utf8(b) {
        Ok(s) => Json::String(s.to_string()),
        Err(_) => json!({ "base64": base64_encode(b) }),
    }
}

/// A complete value as JSON.
fn value_json(page: &KvPage) -> Json {
    match page {
        KvPage::String { bytes, .. } => bytes_json(bytes),
        KvPage::Hash { fields, .. } => Json::Object(
            fields
                .iter()
                .map(|(f, v)| (display_key(f), bytes_json(v)))
                .collect::<Map<String, Json>>(),
        ),
        KvPage::List { items, .. } => Json::Array(items.iter().map(|i| bytes_json(i)).collect()),
        KvPage::Set { members, .. } => Json::Array(members.iter().map(|m| bytes_json(m)).collect()),
        KvPage::ZSet { members, .. } => Json::Array(
            members
                .iter()
                .map(|(m, score)| json!([bytes_json(m), score]))
                .collect(),
        ),
        KvPage::Stream { entries, .. } => Json::Array(
            entries
                .iter()
                .map(|(id, fields)| {
                    let fields: Map<String, Json> = fields
                        .iter()
                        .map(|(f, v)| (display_key(f), bytes_json(v)))
                        .collect();
                    json!({ "id": id, "fields": fields })
                })
                .collect(),
        ),
    }
}

/// Bytes a page holds, to bound a value.
fn page_bytes(page: &KvPage) -> usize {
    match page {
        KvPage::String { bytes, .. } => bytes.len(),
        KvPage::Hash { fields, .. } => fields.iter().map(|(f, v)| f.len() + v.len()).sum(),
        KvPage::List { items, .. } => items.iter().map(Vec::len).sum(),
        KvPage::Set { members, .. } => members.iter().map(Vec::len).sum(),
        KvPage::ZSet { members, .. } => members.iter().map(|(m, _)| m.len() + 8).sum(),
        KvPage::Stream { entries, .. } => entries
            .iter()
            .map(|(id, f)| id.len() + f.iter().map(|(a, b)| a.len() + b.len()).sum::<usize>())
            .sum(),
    }
}

/// The value of `key`, every page of it; `None` for a type without pages (module types).
async fn read_value(
    kv: &dyn KeyValueConn,
    db: u32,
    key: &[u8],
    ty: &KeyType,
) -> Result<Option<Json>, DriverError> {
    if matches!(ty, KeyType::Json | KeyType::Other(_)) {
        return Ok(None);
    }
    let mut value = kv.page(db, key, KvCursor::Start, VALUE_PAGE).await?;
    let mut seen = std::collections::HashSet::new();
    value.names(&mut seen);
    while let Some(next) = value.next() {
        if page_bytes(&value) > MAX_VALUE_BYTES {
            return Err(DriverError::new(
                ErrorKind::Other,
                format!(
                    "The value of {} is larger than {} MB: export it from the console",
                    display_key(key),
                    MAX_VALUE_BYTES >> 20
                ),
            ));
        }
        let page = kv.page(db, key, next, VALUE_PAGE).await?;
        if page.is_empty() && page.next().is_none() {
            break;
        }
        value.append_dedup(page, &mut seen);
    }
    Ok(Some(value_json(&value)))
}

impl Shared {
    /// The keys of `db` on `conn` matching `pattern`, as rows `key, type, ttl, value`.
    pub(crate) async fn key_rows(
        self: &Arc<Self>,
        conn: &str,
        db: u32,
        pattern: &[u8],
    ) -> Result<Records, DriverError> {
        let key = SessionKey::meta(conn);
        let io = self.live_io(conn).ok_or_else(not_connected)?;
        let connection = self.connected(&key, &io).await.ok_or_else(not_connected)?;
        if connection.key_value().is_none() {
            return Err(DriverError::new(
                ErrorKind::Unsupported,
                "this connection has no keys",
            ));
        }
        let columns = vec![
            ColumnMeta::new("key", "text", ColumnKind::Text, DisplayHint::Plain),
            ColumnMeta::new("type", "text", ColumnKind::Text, DisplayHint::Plain),
            ColumnMeta::new("ttl", "int8", ColumnKind::Int, DisplayHint::Number),
            ColumnMeta::new("value", "json", ColumnKind::Text, DisplayHint::Json),
        ];
        let (tx, rx) = mpsc::channel(2);
        let pattern = if pattern.is_empty() { b"*" } else { pattern }.to_vec();
        tokio::spawn(async move {
            let Some(kv) = connection.key_value() else {
                return;
            };
            let mut cursor = 0u64;
            let mut line = 0u64;
            let mut batch = Vec::new();
            loop {
                let (keys, next) = match kv.scan(db, &pattern, cursor, SCAN_COUNT).await {
                    Ok(page) => page,
                    Err(e) => {
                        let _ = tx.send(Err(e.message)).await;
                        return;
                    }
                };
                for info in keys {
                    let value = match read_value(kv, db, &info.name, &info.ty).await {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = tx.send(Err(e.message)).await;
                            return;
                        }
                    };
                    line += 1;
                    batch.push((
                        line,
                        vec![
                            Value::Text(display_key(&info.name)),
                            Value::Text(info.ty.label().to_string()),
                            info.ttl_ms.map_or(Value::Null, Value::Int),
                            value.map_or(Value::Null, |v| Value::Text(v.to_string())),
                        ],
                    ));
                    if batch.len() >= KEYS_PER_BATCH
                        && tx.send(Ok(std::mem::take(&mut batch))).await.is_err()
                    {
                        return;
                    }
                }
                cursor = next;
                if cursor == 0 {
                    break;
                }
            }
            if !batch.is_empty() {
                let _ = tx.send(Ok(batch)).await;
            }
        });
        Ok(Records::Rows {
            columns,
            reader: rx,
            typed_text: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_as_json() {
        let hash = KvPage::Hash {
            fields: vec![(b"a".to_vec(), b"1".to_vec()), (b"b".to_vec(), vec![0xff])],
            cursor: 0,
        };
        assert_eq!(
            value_json(&hash).to_string(),
            "{\"a\":\"1\",\"b\":{\"base64\":\"/w==\"}}"
        );
        let z = KvPage::ZSet {
            members: vec![(b"m".to_vec(), 1.5)],
            start: 0,
            total: 1,
        };
        assert_eq!(value_json(&z).to_string(), "[[\"m\",1.5]]");
    }
}
