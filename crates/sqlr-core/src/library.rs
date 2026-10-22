//! Types for the local library: query history and snippets.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryOutcome {
    Rows(u64),
    Affected(u64),
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    /// 0 until stored.
    pub id: i64,
    pub conn_id: String,
    /// The server database the statement ran on, when the tab was bound to one.
    pub database: Option<String>,
    pub sql: String,
    /// Unix milliseconds.
    pub started_at_ms: i64,
    pub duration_ms: u64,
    pub outcome: HistoryOutcome,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryQuery {
    /// Case-insensitive substring; empty matches everything.
    pub text: String,
    pub conn_id: Option<String>,
    /// 0 means 200.
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub id: String,
    pub name: String,
    /// `/`-separated folder; empty is the top level.
    pub folder: String,
    /// `None` is a global snippet.
    pub conn_id: Option<String>,
    pub sql: String,
    pub updated_at_ms: i64,
}
