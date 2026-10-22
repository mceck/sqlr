//! Values: type, TTL and size of a key, one page of its value, and edits.
//!
//! Collections page with `HSCAN`/`SSCAN` (opaque cursor), `LRANGE`/`ZRANGE` (index) and
//! `XRANGE` (id); a string with `GETRANGE`. A page never holds more than `count` elements
//! (`HSCAN` and `SSCAN` may exceed it by one bucket), and nothing here loads a whole value.

use redis::{Cmd, RedisResult, Value};
use sqlr_core::{
    DriverError, ErrorKind, KeyInfo, KeyType, KvCursor, KvEdit, KvPage, STRING_PAGE_BYTES,
    StreamEntry,
};

use crate::connect::{Inner, Role};

/// The bytes of a string-like reply.
pub(crate) fn bytes_of(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::BulkString(b) => Some(b.clone()),
        Value::SimpleString(s) => Some(s.clone().into_bytes()),
        Value::VerbatimString { text, .. } => Some(text.clone().into_bytes()),
        Value::Int(i) => Some(i.to_string().into_bytes()),
        Value::Double(d) => Some(d.to_string().into_bytes()),
        Value::Okay => Some(b"OK".to_vec()),
        _ => None,
    }
}

/// The text of a string-like reply.
pub(crate) fn text_of(value: &Value) -> Option<String> {
    bytes_of(value).map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn int_of(value: &Value) -> Option<i64> {
    match value {
        Value::Int(i) => Some(*i),
        other => text_of(other)?.parse().ok(),
    }
}

fn unexpected(what: &str) -> DriverError {
    DriverError::new(
        ErrorKind::Protocol,
        format!("Unexpected reply from the server: {what}"),
    )
}

fn missing_key() -> DriverError {
    DriverError::new(ErrorKind::Other, "The key does not exist")
}

fn cmd(name: &str, key: &[u8]) -> Cmd {
    let mut c = redis::cmd(name);
    c.arg(key);
    c
}

/// Fields of a hash reply: a flat `field, value, …` array (RESP2, and `HSCAN` in RESP3) or
/// a map.
fn pairs_of(value: Value) -> Vec<(Vec<u8>, Vec<u8>)> {
    match value {
        Value::Map(pairs) => pairs
            .iter()
            .filter_map(|(k, v)| Some((bytes_of(k)?, bytes_of(v)?)))
            .collect(),
        Value::Array(items) => items
            .chunks(2)
            .filter_map(|c| Some((bytes_of(c.first()?)?, bytes_of(c.get(1)?)?)))
            .collect(),
        _ => Vec::new(),
    }
}

fn members_of(value: Value) -> Vec<Vec<u8>> {
    match value {
        Value::Array(items) | Value::Set(items) => items.iter().filter_map(bytes_of).collect(),
        _ => Vec::new(),
    }
}

/// `member, score` pairs of a `WITHSCORES` reply: nested pairs (RESP3) or a flat array.
fn scored_of(value: Value) -> Vec<(Vec<u8>, f64)> {
    let Value::Array(items) = value else {
        return Vec::new();
    };
    let score = |v: &Value| match v {
        Value::Double(d) => Some(*d),
        other => text_of(other)?.parse().ok(),
    };
    if items.iter().all(|i| matches!(i, Value::Array(_))) {
        return items
            .iter()
            .filter_map(|pair| {
                let Value::Array(pair) = pair else {
                    return None;
                };
                Some((bytes_of(pair.first()?)?, score(pair.get(1)?)?))
            })
            .collect();
    }
    items
        .chunks(2)
        .filter_map(|c| Some((bytes_of(c.first()?)?, score(c.get(1)?)?)))
        .collect()
}

/// The entries of an `XRANGE` reply.
fn entries_of(value: Value) -> Vec<StreamEntry> {
    let Value::Array(entries) = value else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter_map(|entry| {
            let Value::Array(mut parts) = entry else {
                return None;
            };
            if parts.len() < 2 {
                return None;
            }
            let fields = parts.pop()?;
            let id = text_of(&parts[0])?;
            Some((id, pairs_of(fields)))
        })
        .collect()
}

/// The stream id right after `id`, so a page can start after the last entry of the
/// previous one (works on servers without exclusive ranges).
pub(crate) fn next_stream_id(id: &str) -> String {
    let (ms, seq) = id.split_once('-').unwrap_or((id, "0"));
    let ms: u64 = ms.parse().unwrap_or(0);
    let seq: u64 = seq.parse().unwrap_or(0);
    match seq.checked_add(1) {
        Some(seq) => format!("{ms}-{seq}"),
        None => format!("{}-0", ms.saturating_add(1)),
    }
}

/// `SCAN`-family reply: `[cursor, items]`.
fn scan_reply(value: Value) -> Option<(u64, Value)> {
    let Value::Array(mut parts) = value else {
        return None;
    };
    if parts.len() != 2 {
        return None;
    }
    let items = parts.pop()?;
    let cursor = text_of(&parts[0])?.parse().ok()?;
    Some((cursor, items))
}

async fn type_name(con: &mut redis::aio::MultiplexedConnection, key: &[u8]) -> RedisResult<String> {
    let reply: Value = cmd("TYPE", key).query_async(con).await?;
    Ok(text_of(&reply).unwrap_or_else(|| "none".into()))
}

pub(crate) async fn key_info(
    inner: &Inner,
    db: u32,
    key: &[u8],
) -> Result<Option<KeyInfo>, DriverError> {
    let (ty, ttl, size, length) = inner
        .with(db, Role::Browse, true, |mut con| async move {
            let con = &mut con;
            let mut pipe = redis::pipe();
            pipe.add_command(cmd("TYPE", key));
            pipe.add_command(cmd("PTTL", key));
            let mut usage = redis::cmd("MEMORY");
            usage.arg("USAGE").arg(key);
            pipe.add_command(usage);
            // `MEMORY` may be refused by the server's ACL: that only leaves the size out.
            let replies: Vec<Value> = pipe.ignore_errors().query_async(con).await?;
            let ty = replies.first().and_then(text_of).unwrap_or_default();
            let ttl = replies.get(1).and_then(int_of);
            let size = replies.get(2).and_then(int_of);
            let length_command = match ty.as_str() {
                "string" => Some("STRLEN"),
                "hash" => Some("HLEN"),
                "list" => Some("LLEN"),
                "set" => Some("SCARD"),
                "zset" => Some("ZCARD"),
                "stream" => Some("XLEN"),
                _ => None,
            };
            let length = match length_command {
                Some(name) => cmd(name, key).query_async::<i64>(con).await.ok(),
                None => None,
            };
            Ok::<_, redis::RedisError>((ty, ttl, size, length))
        })
        .await?;
    if ty == "none" || ty.is_empty() {
        return Ok(None);
    }
    Ok(Some(KeyInfo {
        name: key.to_vec(),
        ty: KeyType::from_type_name(&ty),
        ttl_ms: ttl.filter(|t| *t >= 0),
        size: size.and_then(|s| u64::try_from(s).ok()),
        length: length.and_then(|l| u64::try_from(l).ok()),
    }))
}

pub(crate) async fn page(
    inner: &Inner,
    db: u32,
    key: &[u8],
    from: KvCursor,
    count: usize,
) -> Result<KvPage, DriverError> {
    let count = count.max(1);
    let index = match &from {
        KvCursor::Index(i) => (*i).max(0),
        _ => 0,
    };
    let scan_cursor = match &from {
        KvCursor::Scan(c) => *c,
        _ => 0,
    };
    let after = match &from {
        KvCursor::After(id) => Some(id.as_str()),
        _ => None,
    };
    let page = inner
        .with(db, Role::Browse, true, |mut con| async move {
            let con = &mut con;
            let ty = type_name(con, key).await?;
            let page = match ty.as_str() {
                "string" => {
                    let end = index + STRING_PAGE_BYTES as i64 - 1;
                    let mut range = cmd("GETRANGE", key);
                    range.arg(index).arg(end);
                    let mut pipe = redis::pipe();
                    pipe.add_command(range);
                    pipe.add_command(cmd("STRLEN", key));
                    let replies: Vec<Value> = pipe.query_async(con).await?;
                    Some(KvPage::String {
                        bytes: replies.first().and_then(bytes_of).unwrap_or_default(),
                        total_len: replies.get(1).and_then(int_of).unwrap_or(0).max(0) as u64,
                    })
                }
                "hash" | "set" => {
                    let name = if ty == "hash" { "HSCAN" } else { "SSCAN" };
                    let mut scan = cmd(name, key);
                    scan.arg(scan_cursor).arg("COUNT").arg(count);
                    let reply: Value = scan.query_async(con).await?;
                    scan_reply(reply).map(|(cursor, items)| {
                        if ty == "hash" {
                            KvPage::Hash {
                                fields: pairs_of(items),
                                cursor,
                            }
                        } else {
                            KvPage::Set {
                                members: members_of(items),
                                cursor,
                            }
                        }
                    })
                }
                "list" => {
                    let mut range = cmd("LRANGE", key);
                    range.arg(index).arg(index + count as i64 - 1);
                    let mut pipe = redis::pipe();
                    pipe.add_command(range);
                    pipe.add_command(cmd("LLEN", key));
                    let replies: Vec<Value> = pipe.query_async(con).await?;
                    Some(KvPage::List {
                        items: replies.first().cloned().map(members_of).unwrap_or_default(),
                        start: index,
                        total: replies.get(1).and_then(int_of).unwrap_or(0).max(0) as u64,
                    })
                }
                "zset" => {
                    let mut range = cmd("ZRANGE", key);
                    range
                        .arg(index)
                        .arg(index + count as i64 - 1)
                        .arg("WITHSCORES");
                    let mut pipe = redis::pipe();
                    pipe.add_command(range);
                    pipe.add_command(cmd("ZCARD", key));
                    let replies: Vec<Value> = pipe.query_async(con).await?;
                    Some(KvPage::ZSet {
                        members: replies.first().cloned().map(scored_of).unwrap_or_default(),
                        start: index,
                        total: replies.get(1).and_then(int_of).unwrap_or(0).max(0) as u64,
                    })
                }
                "stream" => {
                    let start = after.map_or_else(|| "-".to_string(), next_stream_id);
                    let mut range = cmd("XRANGE", key);
                    // One more than a page tells whether another page follows.
                    range.arg(start).arg("+").arg("COUNT").arg(count + 1);
                    let mut pipe = redis::pipe();
                    pipe.add_command(range);
                    pipe.add_command(cmd("XLEN", key));
                    let replies: Vec<Value> = pipe.query_async(con).await?;
                    let mut entries = replies.first().cloned().map(entries_of).unwrap_or_default();
                    let more = entries.len() > count;
                    entries.truncate(count);
                    Some(KvPage::Stream {
                        next: more
                            .then(|| entries.last().map(|(id, _)| id.clone()))
                            .flatten(),
                        entries,
                        total: replies.get(1).and_then(int_of).unwrap_or(0).max(0) as u64,
                    })
                }
                _ => None,
            };
            Ok::<_, redis::RedisError>((ty, page))
        })
        .await?;
    match page {
        (_, Some(page)) => Ok(page),
        (ty, None) if ty == "none" => Err(missing_key()),
        (ty, None) if ty.is_empty() => Err(unexpected("TYPE")),
        (ty, None) => Err(DriverError::new(
            ErrorKind::Unsupported,
            format!("Values of type `{ty}` have no viewer: use the console"),
        )),
    }
}

fn arg_f64(score: f64) -> String {
    if score.is_infinite() {
        if score > 0.0 { "+inf" } else { "-inf" }.to_string()
    } else {
        format!("{score}")
    }
}

/// The command that applies `edit` to `key`.
fn command_for(key: &[u8], edit: &KvEdit) -> Result<Cmd, DriverError> {
    Ok(match edit {
        KvEdit::SetString(value) => {
            let mut c = cmd("SET", key);
            c.arg(value.as_slice()).arg("KEEPTTL");
            c
        }
        KvEdit::HashSet(field, value) => {
            let mut c = cmd("HSET", key);
            c.arg(field.as_slice()).arg(value.as_slice());
            c
        }
        KvEdit::HashDel(field) => {
            let mut c = cmd("HDEL", key);
            c.arg(field.as_slice());
            c
        }
        KvEdit::ListSet(index, value) => {
            let mut c = cmd("LSET", key);
            c.arg(*index).arg(value.as_slice());
            c
        }
        KvEdit::ListPush { left, value } => {
            let mut c = cmd(if *left { "LPUSH" } else { "RPUSH" }, key);
            c.arg(value.as_slice());
            c
        }
        KvEdit::SetAdd(member) => {
            let mut c = cmd("SADD", key);
            c.arg(member.as_slice());
            c
        }
        KvEdit::SetRem(member) => {
            let mut c = cmd("SREM", key);
            c.arg(member.as_slice());
            c
        }
        KvEdit::ZAdd(member, score) => {
            if score.is_nan() {
                return Err(DriverError::new(
                    ErrorKind::Syntax,
                    "The score is not a number",
                ));
            }
            let mut c = cmd("ZADD", key);
            c.arg(arg_f64(*score)).arg(member.as_slice());
            c
        }
        KvEdit::ZRem(member) => {
            let mut c = cmd("ZREM", key);
            c.arg(member.as_slice());
            c
        }
        KvEdit::XAdd(fields) => {
            if fields.is_empty() {
                return Err(DriverError::new(
                    ErrorKind::Syntax,
                    "A stream entry needs at least one field",
                ));
            }
            let mut c = cmd("XADD", key);
            c.arg("*");
            for (field, value) in fields {
                c.arg(field.as_slice()).arg(value.as_slice());
            }
            c
        }
        KvEdit::XDel(id) => {
            let mut c = cmd("XDEL", key);
            c.arg(id.as_str());
            c
        }
        KvEdit::Expire(Some(ms)) => {
            let mut c = cmd("PEXPIRE", key);
            c.arg(*ms);
            c
        }
        KvEdit::Expire(None) => cmd("PERSIST", key),
        KvEdit::Rename(new) => {
            let mut c = cmd("RENAMENX", key);
            c.arg(new.as_slice());
            c
        }
        KvEdit::Delete => cmd("UNLINK", key),
        KvEdit::ListRem { .. } | KvEdit::Create(_) => {
            return Err(DriverError::new(
                ErrorKind::Syntax,
                "This edit runs as a transaction",
            ));
        }
    })
}

pub(crate) async fn edit(
    inner: &Inner,
    db: u32,
    key: &[u8],
    edit: KvEdit,
) -> Result<(), DriverError> {
    inner.check_write()?;
    match &edit {
        KvEdit::Create(first) => return create(inner, db, key, first).await,
        KvEdit::ListRem { index, value } => {
            return list_remove(inner, db, key, *index, value).await;
        }
        _ => {}
    }
    let command = command_for(key, &edit)?;
    // Edits never run twice: a lost connection is reported, not retried.
    let command = &command;
    let reply: Value = inner
        .with(db, Role::Browse, false, |mut con| async move {
            let con = &mut con;
            command.query_async(con).await
        })
        .await?;
    match (&edit, int_of(&reply)) {
        (KvEdit::Expire(Some(_)), Some(0)) => Err(missing_key()),
        (KvEdit::Rename(_), Some(0)) => Err(DriverError::new(
            ErrorKind::Other,
            "The new key name already exists",
        )),
        _ => Ok(()),
    }
}

/// What must hold, checked inside a `WATCH`, before a transaction writes.
enum Guard<'a> {
    /// The key does not exist.
    Absent,
    /// The list still holds `value` at `index`.
    ListItem { index: i64, value: &'a [u8] },
}

/// Runs `writes` as a `MULTI`/`EXEC` on a connection of its own, after `guard` held under
/// `WATCH`: if the key changes in between, the transaction aborts and nothing is written.
async fn guarded(
    inner: &Inner,
    db: u32,
    key: &[u8],
    guard: Guard<'_>,
    writes: Vec<Cmd>,
) -> Result<(), DriverError> {
    // `WATCH` belongs to a connection: a shared multiplexed one would watch for everybody.
    let mut con = inner.open_private(db).await?;
    let outcome: RedisResult<Result<(), DriverError>> = async {
        cmd("WATCH", key).query_async::<()>(&mut con).await?;
        let refused = match guard {
            Guard::Absent => {
                let exists: i64 = cmd("EXISTS", key).query_async(&mut con).await?;
                (exists != 0).then(|| DriverError::new(ErrorKind::Other, "The key already exists"))
            }
            Guard::ListItem { index, value } => {
                let mut lindex = cmd("LINDEX", key);
                lindex.arg(index);
                let item: Value = lindex.query_async(&mut con).await?;
                (bytes_of(&item).as_deref() != Some(value)).then(|| {
                    DriverError::new(
                        ErrorKind::Other,
                        "The list changed since it was shown: reload it",
                    )
                })
            }
        };
        if let Some(e) = refused {
            redis::cmd("UNWATCH").query_async::<()>(&mut con).await?;
            return Ok(Err(e));
        }
        let mut pipe = redis::pipe();
        pipe.atomic();
        for write in writes {
            pipe.add_command(write).ignore();
        }
        let reply: Value = pipe.query_async(&mut con).await?;
        Ok(if matches!(reply, Value::Nil) {
            Err(DriverError::new(
                ErrorKind::Other,
                "The key changed while it was edited: reload it",
            ))
        } else {
            Ok(())
        })
    }
    .await;
    outcome.map_err(crate::errors::map_error)?
}

/// Creates `key` with `first`, refusing to touch a key that exists.
async fn create(inner: &Inner, db: u32, key: &[u8], first: &KvEdit) -> Result<(), DriverError> {
    let exists = || DriverError::new(ErrorKind::Other, "The key already exists");
    match first {
        KvEdit::SetString(value) => {
            let mut set = cmd("SET", key);
            set.arg(value.as_slice()).arg("NX");
            let set = &set;
            let reply: Value = inner
                .with(db, Role::Browse, false, |mut con| async move {
                    let con = &mut con;
                    set.query_async(con).await
                })
                .await?;
            if matches!(reply, Value::Nil) {
                return Err(exists());
            }
            Ok(())
        }
        KvEdit::HashSet(..)
        | KvEdit::ListPush { .. }
        | KvEdit::SetAdd(_)
        | KvEdit::ZAdd(..)
        | KvEdit::XAdd(_) => {
            let write = command_for(key, first)?;
            guarded(inner, db, key, Guard::Absent, vec![write]).await
        }
        _ => Err(DriverError::new(
            ErrorKind::Syntax,
            "A new key starts with a string, a field, an item, a member or an entry",
        )),
    }
}

/// Removes the list item at `index` if it still equals `value`: `LSET` puts a unique
/// tombstone there and `LREM` takes the tombstone out, in one transaction.
async fn list_remove(
    inner: &Inner,
    db: u32,
    key: &[u8],
    index: i64,
    value: &[u8],
) -> Result<(), DriverError> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tombstone = format!("\0sqlr-removed-{nanos}-{n}").into_bytes();
    let mut set = cmd("LSET", key);
    set.arg(index).arg(tombstone.as_slice());
    let mut rem = cmd("LREM", key);
    rem.arg(1).arg(tombstone.as_slice());
    guarded(
        inner,
        db,
        key,
        Guard::ListItem { index, value },
        vec![set, rem],
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_ids_increment() {
        assert_eq!(next_stream_id("5-0"), "5-1");
        assert_eq!(next_stream_id("5-17"), "5-18");
        assert_eq!(next_stream_id("5"), "5-1");
        assert_eq!(next_stream_id(&format!("5-{}", u64::MAX)), "6-0");
    }

    #[test]
    fn replies_parse_in_both_protocols() {
        let b = |s: &str| Value::BulkString(s.as_bytes().to_vec());
        let flat = Value::Array(vec![b("f"), b("1"), b("g"), b("2")]);
        let map = Value::Map(vec![(b("f"), b("1")), (b("g"), b("2"))]);
        assert_eq!(pairs_of(flat), pairs_of(map));
        let resp3 = Value::Array(vec![
            Value::Array(vec![b("m"), Value::Double(1.5)]),
            Value::Array(vec![b("n"), Value::Double(2.0)]),
        ]);
        let resp2 = Value::Array(vec![b("m"), b("1.5"), b("n"), b("2")]);
        assert_eq!(scored_of(resp3), scored_of(resp2));
        let entries = entries_of(Value::Array(vec![Value::Array(vec![
            b("1-0"),
            Value::Array(vec![b("k"), b("v")]),
        ])]));
        assert_eq!(
            entries,
            vec![("1-0".to_string(), vec![(b"k".to_vec(), b"v".to_vec())])]
        );
        assert_eq!(
            scan_reply(Value::Array(vec![b("42"), Value::Array(vec![])])).map(|(c, _)| c),
            Some(42)
        );
    }

    #[test]
    fn scores_render_for_the_server() {
        assert_eq!(arg_f64(1.5), "1.5");
        assert_eq!(arg_f64(f64::INFINITY), "+inf");
        assert_eq!(arg_f64(f64::NEG_INFINITY), "-inf");
    }
}
