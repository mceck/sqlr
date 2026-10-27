//! Structured tool errors. Nothing here includes a secret, connection value or stack trace.

use rmcp::model::CallToolResult;
use serde_json::json;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

impl ToolError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }

    pub fn not_exposed(conn: &str) -> Self {
        Self::new(
            "not_exposed",
            format!("The connection \"{conn}\" is not exposed to agents"),
        )
    }

    pub fn read_only(what: &str) -> Self {
        Self::new(
            "read_only",
            format!("{what} is not allowed: the agent session is read-only"),
        )
    }

    pub fn secret_required(conn: &str, fields: &[String]) -> Self {
        Self::new(
            "secret_required",
            format!(
                "The connection \"{conn}\" needs secret fields: {}",
                fields.join(", ")
            ),
        )
    }

    pub fn denied() -> Self {
        Self::new("denied", "The person denied the request")
    }

    pub fn timeout() -> Self {
        Self::new("timeout", "The approval timed out and was denied")
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    pub fn conflict(current_revision: u64) -> Self {
        Self::new(
            "conflict",
            format!("The tab changed since it was read (revision {current_revision})"),
        )
    }

    pub fn busy() -> Self {
        Self::new("busy", "The tab is already running a query")
    }

    pub fn tab_limit() -> Self {
        Self::new(
            "busy",
            format!(
                "This MCP session already opened {} tabs: reuse the open tabs",
                crate::state::MAX_AGENT_TABS
            ),
        )
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new("unsupported", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("internal", message)
    }

    pub fn to_result(&self) -> CallToolResult {
        CallToolResult::structured_error(json!({
            "error": { "code": self.code, "message": self.message }
        }))
    }
}

pub type ToolResult<T> = Result<T, ToolError>;

/// Replaces credentials in free-form error text before it reaches a log or the model:
/// the password of every URL ([`sqlr_core::url::redact_url`]) and the value of every
/// `password=`, `passwd=`, `token=` and `secret=`, whatever their case.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for piece in text.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        let token = word.trim_end_matches([',', ';', '.', ')', ']', '>', '"', '\'']);
        if token.contains("://") {
            out.push_str(&sqlr_core::url::redact_url(token));
            out.push_str(&piece[token.len()..]);
        } else {
            out.push_str(piece);
        }
    }
    for key in ["password=", "passwd=", "token=", "secret="] {
        out = redact_values(&out, key);
    }
    out
}

/// `text` with the value after every case-insensitive `key` replaced by `***`.
fn redact_values(text: &str, key: &str) -> String {
    // ASCII lowercasing keeps every byte offset.
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(found) = lower[pos..].find(key) {
        let start = pos + found + key.len();
        let end = text[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '&' | ';' | ','))
            .map_or(text.len(), |i| start + i);
        out.push_str(&text[pos..start]);
        out.push_str("***");
        pos = end;
    }
    out.push_str(&text[pos..]);
    out
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn redact_hides_every_occurrence() {
        let text = "connect postgres://alice:pw1@db1/app failed; retry mysql://bob:pw2@db2/x \
                    password=one token=two PASSWORD=three&secret=four passwd=five token=six";
        let out = redact(text);
        for secret in ["pw1", "pw2", "one", "two", "three", "four", "five", "six"] {
            assert!(!out.contains(secret), "{secret} leaked: {out}");
        }
        assert!(out.contains("postgres://alice:***@db1/app"), "{out}");
        assert!(out.contains("mysql://bob:***@db2/x"), "{out}");
        assert!(out.contains("password=***"), "{out}");
        assert!(out.contains("PASSWORD=***&secret=***"), "{out}");
        assert!(out.contains("token=***"), "{out}");
    }

    #[test]
    fn redact_keeps_text_without_credentials() {
        let text = "relation \"orders\" does not exist at https://example.com/docs";
        assert_eq!(redact(text), text);
    }
}
