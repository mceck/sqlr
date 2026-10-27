//! Driver error type shared across crates.

/// The category of a driver error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ErrorKind {
    /// The server could not be reached or the connection dropped.
    Connection,
    /// Authentication or login failed.
    Auth,
    /// The statement could not be parsed.
    Syntax,
    /// The operation took too long.
    Timeout,
    /// The user or the app cancelled the operation.
    Cancelled,
    /// The server or the session refused the operation (privileges, read-only).
    Permission,
    /// The driver or database does not support the operation.
    Unsupported,
    /// The sidecar protocol or wire protocol was violated.
    Protocol,
    /// Any other failure.
    Other,
}

/// An error reported by a driver, with the category and where it happened.
#[derive(Clone, Debug, PartialEq, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[error("{message}")]
pub struct DriverError {
    /// The category of the failure.
    pub kind: ErrorKind,
    /// The message from the server or driver.
    pub message: String,
    /// 0-based char offset into the query.
    pub position: Option<usize>,
    /// A suggestion to fix the failure, when the driver has one.
    pub hint: Option<String>,
}

impl DriverError {
    /// Creates an error of `kind` with `message`, no position and no hint.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            position: None,
            hint: None,
        }
    }

    /// Sets the position of the error in the query.
    pub fn with_position(mut self, pos: usize) -> Self {
        self.position = Some(pos);
        self
    }

    /// Sets the hint of the error.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}
