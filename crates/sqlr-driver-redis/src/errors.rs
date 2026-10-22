//! Mapping from `redis` errors and server error replies to the shared `DriverError`.

use redis::{ErrorKind as RedisKind, RedisError, ServerErrorKind, Value};
use sqlr_core::{DriverError, ErrorKind, RespValue};

/// Why a cluster endpoint is refused (spec §1.2: Redis Cluster is a non-goal).
pub(crate) const CLUSTER_MESSAGE: &str = "This endpoint is a Redis Cluster node: Redis Cluster is not supported. \
     Connect to a standalone server or a single node instead.";

pub(crate) fn cluster_refused() -> DriverError {
    DriverError::new(ErrorKind::Unsupported, CLUSTER_MESSAGE)
}

/// Whether a server error code means the endpoint is part of a cluster.
pub(crate) fn is_cluster_code(code: &str) -> bool {
    matches!(code, "MOVED" | "ASK" | "CLUSTERDOWN" | "CROSSSLOT")
}

/// The text of a server error reply: its code, then its detail.
pub(crate) fn server_text(code: &str, detail: Option<&str>) -> String {
    match detail {
        Some(detail) if !detail.is_empty() => format!("{code} {detail}"),
        _ => code.to_string(),
    }
}

/// The error reply an error stands for, when the server sent one (a console shows these as
/// replies, not as failures).
pub(crate) fn error_reply(e: &RedisError) -> Option<String> {
    let server = matches!(e.kind(), RedisKind::Server(_) | RedisKind::Extension);
    let code = e.code().filter(|_| server)?;
    Some(server_text(code, e.detail()))
}

/// Whether the connection (not the command) failed: a dropped or reset socket.
pub(crate) fn is_connection_error(e: &RedisError) -> bool {
    e.is_io_error() || e.is_connection_dropped() || e.is_unrecoverable_error()
}

pub(crate) fn map_error(e: RedisError) -> DriverError {
    if let Some(code) = e.code()
        && is_cluster_code(code)
    {
        return cluster_refused();
    }
    match e.kind() {
        RedisKind::AuthenticationFailed => DriverError::new(
            ErrorKind::Auth,
            "Authentication failed: the server refused the user or password",
        ),
        RedisKind::Server(ServerErrorKind::NoPerm) => DriverError::new(
            ErrorKind::Permission,
            format!("Permission denied: {}", reply_text(&e)),
        ),
        RedisKind::Server(ServerErrorKind::ReadOnly) => {
            DriverError::new(ErrorKind::Permission, reply_text(&e))
        }
        RedisKind::Server(_) | RedisKind::Extension => match e.code() {
            Some("WRONGPASS" | "NOAUTH") => DriverError::new(
                ErrorKind::Auth,
                format!("Authentication failed: {}", reply_text(&e)),
            ),
            Some("NOPERM") => DriverError::new(
                ErrorKind::Permission,
                format!("Permission denied: {}", reply_text(&e)),
            ),
            _ => DriverError::new(ErrorKind::Other, reply_text(&e)),
        },
        RedisKind::Io if e.is_timeout() => DriverError::new(
            ErrorKind::Timeout,
            "The Redis server did not answer in time",
        ),
        RedisKind::Io => DriverError::new(
            ErrorKind::Connection,
            format!(
                "Connection to the Redis server failed: {}",
                e.detail().map_or_else(|| e.to_string(), str::to_string)
            ),
        ),
        RedisKind::RESP3NotSupported => DriverError::new(ErrorKind::Protocol, e.to_string()),
        RedisKind::Parse | RedisKind::UnexpectedReturnType => {
            DriverError::new(ErrorKind::Protocol, e.to_string())
        }
        _ => DriverError::new(ErrorKind::Other, e.to_string()),
    }
}

fn reply_text(e: &RedisError) -> String {
    error_reply(e).unwrap_or_else(|| e.to_string())
}

/// A reply as the console shows it.
pub(crate) fn to_resp(value: Value) -> RespValue {
    match value {
        Value::Nil => RespValue::Nil,
        Value::Int(i) => RespValue::Int(i),
        Value::BulkString(bytes) => RespValue::Bulk(bytes),
        Value::Array(items) | Value::Set(items) => {
            RespValue::Array(items.into_iter().map(to_resp).collect())
        }
        Value::SimpleString(text) => RespValue::Simple(text),
        Value::Okay => RespValue::Simple("OK".into()),
        Value::Map(pairs) => RespValue::Map(
            pairs
                .into_iter()
                .map(|(k, v)| (to_resp(k), to_resp(v)))
                .collect(),
        ),
        Value::Attribute { data, .. } => to_resp(*data),
        Value::Double(d) => RespValue::Double(d),
        Value::Boolean(b) => RespValue::Bool(b),
        Value::VerbatimString { text, .. } => RespValue::Verbatim(text),
        Value::BigNumber(n) => RespValue::Simple(format!("{n:?}")),
        Value::Push { data, .. } => RespValue::Array(data.into_iter().map(to_resp).collect()),
        Value::ServerError(e) => RespValue::Error(server_text(e.code(), e.details())),
        // `Value` is non-exhaustive.
        other => RespValue::Simple(format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_codes_are_recognised() {
        for code in ["MOVED", "ASK", "CLUSTERDOWN", "CROSSSLOT"] {
            assert!(is_cluster_code(code), "{code}");
        }
        assert!(!is_cluster_code("WRONGTYPE"));
        assert!(!is_cluster_code("ERR"));
        let e = cluster_refused();
        assert_eq!(e.kind, ErrorKind::Unsupported);
        assert!(e.message.contains("Redis Cluster"));
    }

    #[test]
    fn server_text_joins_code_and_detail() {
        assert_eq!(
            server_text("ERR", Some("unknown command")),
            "ERR unknown command"
        );
        assert_eq!(server_text("NOAUTH", None), "NOAUTH");
        assert_eq!(server_text("ERR", Some("")), "ERR");
    }

    #[test]
    fn replies_convert_by_type() {
        assert_eq!(to_resp(Value::Okay), RespValue::Simple("OK".into()));
        assert_eq!(to_resp(Value::Nil), RespValue::Nil);
        assert_eq!(
            to_resp(Value::Array(vec![Value::Int(1), Value::Boolean(true)])),
            RespValue::Array(vec![RespValue::Int(1), RespValue::Bool(true)])
        );
        assert_eq!(
            to_resp(Value::Map(vec![(
                Value::BulkString(b"k".to_vec()),
                Value::Double(1.5)
            )])),
            RespValue::Map(vec![(
                RespValue::Bulk(b"k".to_vec()),
                RespValue::Double(1.5)
            )])
        );
    }
}
