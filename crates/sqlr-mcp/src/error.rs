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
