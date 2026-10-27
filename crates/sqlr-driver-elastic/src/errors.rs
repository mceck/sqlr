//! Mapping HTTP failures and Elasticsearch error bodies to the shared `DriverError`.

use serde_json::Value;
use sqlr_core::{DriverError, ErrorKind};

/// `e` and its sources, joined by `: `.
fn chain(e: &dyn std::error::Error) -> String {
    let mut message = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        let text = s.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = s.source();
    }
    message
}

/// A transport failure (connect, TLS, timeout).
#[expect(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` function pointer, which receives the error by value"
)]
pub(crate) fn http_error(e: reqwest::Error) -> DriverError {
    let kind = if e.is_timeout() {
        ErrorKind::Timeout
    } else {
        ErrorKind::Connection
    };
    DriverError::new(kind, chain(&e))
}

/// The `type` and `reason` of an error body, from `error` or its first root cause.
pub(crate) fn error_parts(body: &Value) -> (Option<String>, Option<String>) {
    let err = &body["error"];
    if let Some(s) = err.as_str() {
        return (None, Some(s.to_string()));
    }
    let pick = |v: &Value, k: &str| v[k].as_str().map(String::from);
    let root = &err["root_cause"][0];
    (
        pick(err, "type").or_else(|| pick(root, "type")),
        pick(err, "reason").or_else(|| pick(root, "reason")),
    )
}

/// The error for a non-2xx response.
pub(crate) fn status_error(status: u16, body: &Value, text: &str) -> DriverError {
    let (ty, reason) = error_parts(body);
    let reason = reason.unwrap_or_else(|| {
        let t = text.trim();
        if t.is_empty() {
            format!("HTTP {status}")
        } else {
            t.chars().take(500).collect()
        }
    });
    let kind = match status {
        401 => ErrorKind::Auth,
        403 => ErrorKind::Permission,
        400 => ErrorKind::Syntax,
        408 | 504 => ErrorKind::Timeout,
        502 | 503 => ErrorKind::Connection,
        _ => ErrorKind::Other,
    };
    let message = match (kind, ty) {
        (ErrorKind::Auth, _) => format!("Authentication failed: {reason}"),
        (_, Some(ty)) => format!("{ty}: {reason}"),
        (_, None) => reason,
    };
    DriverError::new(kind, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_bodies() {
        let body = serde_json::json!({
            "error": {"root_cause": [{"type": "index_not_found_exception", "reason": "no such index [x]"}],
                      "type": "index_not_found_exception", "reason": "no such index [x]"},
            "status": 404
        });
        let e = status_error(404, &body, "");
        assert_eq!(e.kind, ErrorKind::Other);
        assert_eq!(e.message, "index_not_found_exception: no such index [x]");
        let e = status_error(
            401,
            &serde_json::json!({"error": {"type": "security_exception", "reason": "unable to authenticate user [elastic]"}}),
            "",
        );
        assert_eq!(e.kind, ErrorKind::Auth);
        assert!(e.message.starts_with("Authentication failed: unable"));
        let e = status_error(400, &Value::Null, "bad");
        assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Syntax, "bad"));
    }
}
