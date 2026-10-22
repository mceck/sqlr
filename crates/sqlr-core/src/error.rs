//! Driver error type shared across crates.

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ErrorKind {
    Connection,
    Auth,
    Syntax,
    Timeout,
    Cancelled,
    Permission,
    Unsupported,
    Protocol,
    Other,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[error("{message}")]
pub struct DriverError {
    pub kind: ErrorKind,
    pub message: String,
    /// 0-based char offset into the query.
    pub position: Option<usize>,
    pub hint: Option<String>,
}

impl DriverError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            position: None,
            hint: None,
        }
    }

    pub fn with_position(mut self, pos: usize) -> Self {
        self.position = Some(pos);
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}
