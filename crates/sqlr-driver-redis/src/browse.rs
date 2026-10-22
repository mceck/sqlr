//! Browsing: `SCAN` with a `MATCH` pattern (never `KEYS`) and the explorer tree.
//!
//! The tree is databases, then key prefix groups (split at the connection's separator),
//! then keys. A group is listed by scanning for its prefix, bounded so a huge keyspace
//! cannot stall the explorer: past the bound the node list ends with a note.

use std::collections::BTreeMap;

use redis::Value;
use sqlr_core::{
    DriverError, KeyInfo, KeyType, KvNodePath, Node, NodeKind, NodePath, SCAN_COUNT, display_key,
    glob_escape,
};

use crate::connect::{Inner, Role};
use crate::values::text_of;

/// Keys one explorer listing looks at, at most.
const BROWSE_MAX_KEYS: usize = 10_000;
/// `SCAN` calls one explorer listing makes, at most (a prefix that matches little makes
/// each call cheap and fruitless).
const BROWSE_MAX_CALLS: usize = 400;
/// Commands in one pipeline when looking up types.
const PIPELINE_CHUNK: usize = 500;

/// One `SCAN` call: the keys and the next cursor.
async fn scan_call(
    con: &mut redis::aio::MultiplexedConnection,
    pattern: Option<&[u8]>,
    cursor: u64,
    count: usize,
) -> redis::RedisResult<(Vec<Vec<u8>>, u64)> {
    let mut scan = redis::cmd("SCAN");
    scan.arg(cursor);
    if let Some(pattern) = pattern {
        scan.arg("MATCH").arg(pattern);
    }
    scan.arg("COUNT").arg(count);
    let reply: Value = scan.query_async(con).await?;
    let parsed = match reply {
        Value::Array(mut parts) if parts.len() == 2 => {
            let keys = parts.pop();
            let cursor = parts.pop().and_then(|c| text_of(&c)?.parse::<u64>().ok());
            cursor.zip(keys)
        }
        _ => None,
    };
    let Some((next, Value::Array(keys))) = parsed else {
        return Err((redis::ErrorKind::Parse, "unexpected SCAN reply").into());
    };
    let keys = keys
        .into_iter()
        .filter_map(|k| match k {
            Value::BulkString(b) => Some(b),
            _ => None,
        })
        .collect();
    Ok((keys, next))
}

/// The type and TTL (milliseconds) of each key; `None` for a key that is gone.
async fn describe(
    con: &mut redis::aio::MultiplexedConnection,
    keys: &[Vec<u8>],
    with_ttl: bool,
) -> redis::RedisResult<Vec<Option<(KeyType, Option<i64>)>>> {
    let mut out = Vec::with_capacity(keys.len());
    for chunk in keys.chunks(PIPELINE_CHUNK) {
        let mut pipe = redis::pipe();
        for key in chunk {
            pipe.cmd("TYPE").arg(key.as_slice());
            if with_ttl {
                pipe.cmd("PTTL").arg(key.as_slice());
            }
        }
        let replies: Vec<Value> = pipe.query_async(con).await?;
        let step = if with_ttl { 2 } else { 1 };
        for pair in replies.chunks(step) {
            let ty = pair.first().and_then(text_of).unwrap_or_default();
            if ty == "none" || ty.is_empty() {
                out.push(None);
                continue;
            }
            let ttl = pair.get(1).and_then(|v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            });
            out.push(Some((
                KeyType::from_type_name(&ty),
                ttl.filter(|t| *t >= 0),
            )));
        }
    }
    Ok(out)
}

/// One page of `pattern` (`*` and the empty pattern match everything).
pub(crate) async fn scan(
    inner: &Inner,
    db: u32,
    pattern: &[u8],
    cursor: u64,
    count: usize,
) -> Result<(Vec<KeyInfo>, u64), DriverError> {
    let matcher = (!pattern.is_empty() && pattern != b"*").then_some(pattern);
    inner
        .with(db, Role::Browse, true, |mut con| async move {
            let con = &mut con;
            let (keys, next) = scan_call(con, matcher, cursor, count.max(1)).await?;
            let described = describe(con, &keys, true).await?;
            let found = keys
                .into_iter()
                .zip(described)
                .filter_map(|(name, d)| {
                    let (ty, ttl_ms) = d?;
                    Some(KeyInfo {
                        ttl_ms,
                        ..KeyInfo::new(name, ty)
                    })
                })
                .collect();
            Ok::<_, redis::RedisError>((found, next))
        })
        .await
}

/// The non-empty databases of `INFO keyspace`: `(index, keys)`.
pub(crate) fn parse_keyspace(info: &str) -> Vec<(u32, u64)> {
    info.lines()
        .filter_map(|line| {
            let (db, rest) = line.trim().split_once(':')?;
            let db: u32 = db.strip_prefix("db")?.parse().ok()?;
            let keys = rest
                .split(',')
                .find_map(|kv| kv.strip_prefix("keys="))?
                .parse()
                .ok()?;
            Some((db, keys))
        })
        .collect()
}

fn count_label(n: u64) -> String {
    if n == 1 {
        "1 key".into()
    } else {
        format!("{n} keys")
    }
}

/// What lies directly under `prefix`: groups (name, keys inside) and leaf keys.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Level {
    pub groups: BTreeMap<Vec<u8>, u64>,
    pub leaves: Vec<Vec<u8>>,
}

/// Splits `keys` (all starting with `prefix`) into the groups and leaves directly under it.
pub(crate) fn group_level(keys: &[Vec<u8>], prefix: &[u8], separator: &[u8]) -> Level {
    let mut level = Level::default();
    for key in keys {
        let Some(rest) = key.strip_prefix(prefix) else {
            continue;
        };
        let split = (!separator.is_empty())
            .then(|| rest.windows(separator.len()).position(|w| w == separator))
            .flatten();
        match split {
            Some(at) => *level.groups.entry(rest[..at].to_vec()).or_insert(0) += 1,
            None => level.leaves.push(key.clone()),
        }
    }
    level.leaves.sort();
    level
}

pub(crate) async fn browse(inner: &Inner, path: &NodePath) -> Result<Vec<Node>, DriverError> {
    if path.is_root() {
        return browse_databases(inner).await;
    }
    let Some(db) = KvNodePath::db_of(path) else {
        return Ok(Vec::new());
    };
    match path.segments().len() {
        1 => browse_prefix(inner, path, db, Vec::new()).await,
        _ => match KvNodePath::prefix_of(path) {
            Some(prefix) => browse_prefix(inner, path, db, prefix).await,
            None => Ok(Vec::new()),
        },
    }
}

async fn browse_databases(inner: &Inner) -> Result<Vec<Node>, DriverError> {
    let configured = inner.configured_db;
    let info = inner
        .with(configured, Role::Browse, true, |mut con| async move {
            let con = &mut con;
            redis::cmd("INFO")
                .arg("keyspace")
                .query_async::<Value>(con)
                .await
        })
        .await?;
    let mut dbs: BTreeMap<u32, u64> = parse_keyspace(&text_of(&info).unwrap_or_default())
        .into_iter()
        .collect();
    dbs.entry(configured).or_insert(0);
    Ok(dbs
        .into_iter()
        .map(|(db, keys)| Node {
            path: KvNodePath::database(db),
            name: format!("db{db}"),
            kind: NodeKind::Database,
            has_children: keys > 0,
            detail: Some(count_label(keys)),
        })
        .collect())
}

async fn browse_prefix(
    inner: &Inner,
    parent: &NodePath,
    db: u32,
    prefix: Vec<u8>,
) -> Result<Vec<Node>, DriverError> {
    let mut matcher = glob_escape(&prefix);
    matcher.push(b'*');
    let matcher = (!prefix.is_empty()).then_some(matcher);
    let matcher = matcher.as_deref();
    let (keys, complete) = inner
        .with(db, Role::Browse, true, |mut con| async move {
            let con = &mut con;
            let mut keys: Vec<Vec<u8>> = Vec::new();
            let mut cursor = 0;
            for _ in 0..BROWSE_MAX_CALLS {
                let (batch, next) = scan_call(con, matcher, cursor, SCAN_COUNT).await?;
                keys.extend(batch);
                cursor = next;
                if cursor == 0 || keys.len() >= BROWSE_MAX_KEYS {
                    break;
                }
            }
            Ok::<_, redis::RedisError>((keys, cursor == 0))
        })
        .await?;
    // A scan may return a key twice.
    let mut keys = keys;
    keys.sort();
    keys.dedup();
    let level = group_level(&keys, &prefix, &inner.separator);
    let leaves = &level.leaves;
    let types = inner
        .with(db, Role::Browse, true, |mut con| async move {
            let con = &mut con;
            describe(con, leaves, false).await
        })
        .await?;
    let mut nodes = Vec::new();
    for (name, count) in &level.groups {
        let mut child_prefix = prefix.clone();
        child_prefix.extend_from_slice(name);
        child_prefix.extend_from_slice(&inner.separator);
        nodes.push(Node {
            path: KvNodePath::group(parent, &child_prefix),
            name: if name.is_empty() {
                "(empty)".into()
            } else {
                display_key(name)
            },
            kind: NodeKind::Folder,
            has_children: true,
            detail: Some(count_label(*count)),
        });
    }
    for (key, described) in level.leaves.iter().zip(types) {
        let Some((ty, _)) = described else {
            continue;
        };
        nodes.push(Node {
            path: KvNodePath::key(parent, key),
            name: display_key(&key[prefix.len()..]),
            kind: NodeKind::Table,
            has_children: false,
            detail: Some(ty.label().to_string()),
        });
    }
    if !complete {
        nodes.push(Node {
            path: parent.child("more"),
            name: "…".into(),
            kind: NodeKind::Folder,
            has_children: false,
            detail: Some("more keys: use the key browser".into()),
        });
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(list: &[&str]) -> Vec<Vec<u8>> {
        list.iter().map(|k| k.as_bytes().to_vec()).collect()
    }

    #[test]
    fn keyspace_lines_parse() {
        let info =
            "# Keyspace\r\ndb0:keys=7,expires=1,avg_ttl=0\r\ndb3:keys=1,expires=0,avg_ttl=0\r\n";
        assert_eq!(parse_keyspace(info), [(0, 7), (3, 1)]);
        assert!(parse_keyspace("# Keyspace\r\n").is_empty());
    }

    #[test]
    fn levels_split_at_the_separator() {
        let all = keys(&["user:1:name", "user:1:age", "user:2:name", "user", "plain"]);
        let root = group_level(&all, b"", b":");
        assert_eq!(root.groups.len(), 1);
        assert_eq!(root.groups[b"user".as_slice()], 3);
        assert_eq!(root.leaves, keys(&["plain", "user"]));
        let user = group_level(&all[..3], b"user:", b":");
        assert_eq!(user.groups.len(), 2);
        assert_eq!(user.groups[b"1".as_slice()], 2);
        assert!(user.leaves.is_empty());
        // No separator: everything is a leaf.
        let flat = group_level(&all, b"", b"");
        assert!(flat.groups.is_empty());
        assert_eq!(flat.leaves.len(), 5);
        // A separator of several bytes; an empty group name.
        let wide = group_level(&keys(&["a::b", "a::::c", "x"]), b"", b"::");
        assert_eq!(wide.groups[b"a".as_slice()], 2);
        let empty = group_level(&keys(&[":x"]), b"", b":");
        assert_eq!(empty.groups[b"".as_slice()], 1);
    }
}
