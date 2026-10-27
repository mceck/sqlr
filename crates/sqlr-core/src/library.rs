//! Types for the local library: query history and snippets.

/// How a history entry ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryOutcome {
    /// It returned this many rows.
    Rows(u64),
    /// It changed this many rows.
    Affected(u64),
    /// It failed with this message.
    Failed(String),
    /// It was cancelled.
    Cancelled,
}

/// A statement run earlier, as the history keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    /// 0 until stored.
    pub id: i64,
    /// The connection it ran on.
    pub conn_id: String,
    /// The server database the statement ran on, when the tab was bound to one.
    pub database: Option<String>,
    /// The statement text.
    pub sql: String,
    /// Unix milliseconds.
    pub started_at_ms: i64,
    /// How long it took, in milliseconds.
    pub duration_ms: u64,
    /// How it ended.
    pub outcome: HistoryOutcome,
}

/// A search of the history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryQuery {
    /// Case-insensitive substring; empty matches everything.
    pub text: String,
    /// Only entries of this connection; `None` for all.
    pub conn_id: Option<String>,
    /// 0 means 200.
    pub limit: usize,
}

/// A saved statement of the snippet library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    /// Stable identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// `/`-separated folder; empty is the top level.
    pub folder: String,
    /// `None` is a global snippet.
    pub conn_id: Option<String>,
    /// The snippet text.
    pub sql: String,
    /// When it was last changed, in Unix milliseconds.
    pub updated_at_ms: i64,
}
