//! The boundary to the running application. Queries and catalog reads go to the engine
//! directly; everything that needs the UI thread (configuration stores, editor tabs,
//! approvals) goes through this trait.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlr_core::{ConnectionConfig, ConnectionProfile, DriverManifest, Node, Secrets, SettingValue};

use crate::error::ToolResult;

/// The connection values an agent may never see, handed to the MCP layer only to connect.
#[derive(Clone)]
pub struct AgentConnection {
    pub config: ConnectionConfig,
    pub secrets: Secrets,
    pub profile: ConnectionProfile,
    pub manifest: DriverManifest,
}

#[derive(Clone, Debug, Serialize)]
pub struct SettingEntry {
    pub id: String,
    pub section: String,
    pub label: String,
    pub description: String,
    pub kind: Value,
    pub default: Value,
    pub allowed_levels: Vec<String>,
    pub safety: String,
    pub agent_readable: bool,
    pub agent_writable: bool,
    /// Values by `global`, `profile:<id>` and `connection:<id>`.
    pub values: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct KeymapEntry {
    pub action: String,
    pub label: String,
    pub group: String,
    pub chords: Vec<String>,
    pub differs_from_preset: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingLevel {
    Global,
    Profile,
    Connection,
}

impl SettingLevel {
    pub fn key(&self, id: Option<&str>) -> String {
        match self {
            Self::Global => "global".into(),
            Self::Profile => format!("profile:{}", id.unwrap_or_default()),
            Self::Connection => format!("connection:{}", id.unwrap_or_default()),
        }
    }
}

/// A connection save the tool already merged and validated. The host writes it through the
/// store worker and updates the UI.
#[derive(Clone)]
pub struct ConnectionWrite {
    pub config: ConnectionConfig,
    pub secrets: BTreeMap<String, String>,
    pub remove_secrets: Vec<String>,
    pub save_secrets: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FolderOp {
    Create { name: String },
    Rename { from: String, to: String },
    Move { name: String, target: MoveTarget },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum MoveTarget {
    End,
    Before { id: String },
    After { id: String },
    Into { id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Write,
    Destructive,
    ConfigurationDelete,
    SnippetDelete,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Self::Write => "Write",
            Self::Destructive => "Destructive",
            Self::ConfigurationDelete => "Configuration delete",
            Self::SnippetDelete => "Snippet delete",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub session_id: String,
    pub client_name: String,
    pub client_version: String,
    pub risk: Risk,
    pub connection: Option<String>,
    pub profile: Option<String>,
    pub database: Option<String>,
    pub exposure: Option<String>,
    pub statement: Option<String>,
    /// A configuration target's summary and consequences instead of a statement.
    pub target: Option<String>,
    pub consequences: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TabSummary {
    pub id: u64,
    pub title: String,
    pub connection_id: Option<String>,
    pub database: Option<String>,
    pub dirty: bool,
    pub revision: u64,
    pub busy: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct TabDetail {
    pub id: u64,
    pub title: String,
    pub connection_id: Option<String>,
    pub database: Option<String>,
    pub dirty: bool,
    pub revision: u64,
    pub busy: bool,
    pub text: String,
    pub caret: usize,
    pub selection: Option<(usize, usize)>,
    /// The engine results of the tab's most recent run, whoever started it: one per
    /// executed statement, in order. The MCP layer shows them for exposed Connections only.
    pub last_run: Vec<u64>,
    /// Whether the tab's Session holds an open transaction.
    pub transaction_open: bool,
    /// The selected cells of the result grid the tab shows, in the result value mapping,
    /// at most [`crate::results::MAX_SELECTION_CELLS`] of them.
    pub grid_selection: Vec<GridCell>,
}

/// One selected cell of a tab's result grid.
#[derive(Clone, Debug, Serialize)]
pub struct GridCell {
    pub column: String,
    /// The row's index in the result, as `get_query_result` offsets count it.
    pub row: usize,
    pub value: Value,
}

/// A tab's result, resolved for paging: the engine result the tab's grid shows.
#[derive(Clone, Debug)]
pub struct TabResultRef {
    pub connection_id: Option<String>,
    pub query: u64,
}

/// A new SQL Editor tab an Agent opens for the person.
#[derive(Clone, Debug)]
pub struct OpenTab {
    pub connection_id: String,
    pub database: Option<String>,
    pub text: String,
    pub title: Option<String>,
}

/// What `show_in_app` brings to the front. The MCP layer has already checked exposure,
/// resolved the Node and obtained any Approval.
#[derive(Clone, Debug)]
pub enum ShowView {
    /// Expands the Explorer to the Node at `path` and selects it.
    Explorer {
        connection_id: String,
        path: Vec<String>,
    },
    /// Opens or focuses the Node's DDL viewer.
    Ddl { connection_id: String, node: Node },
    /// Opens the object's data tab with its first page.
    TableData { connection_id: String, node: Node },
    /// Opens an ER diagram of `tables` in `schema`.
    ErDiagram {
        connection_id: String,
        database: Option<String>,
        schema: String,
        tables: Vec<String>,
    },
    /// Runs Explain of `statement` in the tab's Plan pane, on the tab's Session.
    Explain {
        tab_id: u64,
        statement: String,
        analyze: bool,
    },
}

/// What `show_in_app` did: the tab it opened or focused, and whether it is a new tab.
#[derive(Clone, Copy, Debug, Default)]
pub struct Shown {
    pub tab_id: Option<u64>,
    pub opened: bool,
}

#[derive(Clone, Debug)]
pub enum TabEdit {
    Replace(String),
    Edits(Vec<EditRange>),
}

#[derive(Clone, Debug)]
pub struct EditRange {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Clone, Copy, Debug)]
pub enum RunTarget {
    Script,
    Statement(usize),
    /// The current selection; `invalid` without one.
    Selection,
    /// The selection, or the statement at the caret (what Explain uses).
    Caret,
}

/// Everything the MCP layer needs from the running app.
#[async_trait]
pub trait AgentHost: Send + Sync {
    async fn connection(&self, id: &str) -> ToolResult<AgentConnection>;
    /// Every saved connection, as saved (internal use only).
    async fn connections(&self) -> ToolResult<Vec<ConnectionConfig>>;
    async fn folders(&self) -> ToolResult<Vec<String>>;
    async fn profiles(&self) -> ToolResult<Vec<ConnectionProfile>>;
    async fn save_profile(&self, profile: ConnectionProfile) -> ToolResult<()>;
    async fn delete_profile(&self, id: &str) -> ToolResult<()>;
    async fn drivers(&self) -> ToolResult<Vec<DriverManifest>>;
    /// Which of the driver's secret fields hold a stored value.
    async fn secret_presence(&self, id: &str) -> ToolResult<BTreeMap<String, bool>>;
    async fn settings(&self) -> ToolResult<Vec<SettingEntry>>;
    async fn set_setting(
        &self,
        id: &str,
        level: SettingLevel,
        level_id: Option<&str>,
        value: Option<SettingValue>,
    ) -> ToolResult<()>;
    async fn keymap(&self) -> ToolResult<Vec<KeymapEntry>>;
    async fn set_keymap(&self, action: &str, chords: Option<Vec<String>>) -> ToolResult<()>;
    async fn write_connection(&self, write: ConnectionWrite) -> ToolResult<()>;
    async fn folder(&self, op: FolderOp) -> ToolResult<()>;
    /// Deletes a configuration target after the approval the caller obtained.
    async fn delete(&self, target: &str, id: &str) -> ToolResult<()>;
    /// Shows the in-app approval dialog; `Ok(())` means allow, `Denied`/`Timeout` are
    /// returned as the matching tool errors.
    async fn approve(&self, request: ApprovalRequest) -> ToolResult<()>;
    async fn tabs(&self) -> ToolResult<Vec<TabSummary>>;
    async fn tab(&self, id: u64) -> ToolResult<TabDetail>;
    async fn edit_tab(
        &self,
        id: u64,
        expected_revision: u64,
        edit: TabEdit,
    ) -> ToolResult<TabDetail>;
    /// The statements a run of `target` would execute, from the tab's current buffer.
    async fn tab_statements(&self, id: u64, target: RunTarget) -> ToolResult<Vec<String>>;
    /// Starts a run in the tab's Session, through the person's execution flow, and puts the
    /// results in its grid. `statements` are the ones the caller checked and had approved:
    /// a buffer that changed since answers `conflict` and runs nothing. The run starts
    /// once; the answer lists the engine results started so far.
    async fn run_tab(
        &self,
        id: u64,
        target: RunTarget,
        max_rows: usize,
        statements: Vec<String>,
    ) -> ToolResult<Vec<u64>>;
    /// Resolves a result of the tab's most recent run; `not_found` once the tab ran again
    /// or closed.
    async fn tab_result(&self, id: u64, query: u64) -> ToolResult<TabResultRef>;
    /// Opens a new unsaved SQL Editor tab and makes it active.
    async fn open_tab(&self, open: OpenTab) -> ToolResult<TabDetail>;
    /// Brings a view to the front for the person, with a toast naming it. Without
    /// `open_allowed` (the MCP session reached its tab cap) a view that would open a new
    /// tab opens nothing and answers [`crate::error::ToolError::tab_limit`]; focusing an
    /// open tab and the views that open none still work.
    async fn show(&self, view: ShowView, open_allowed: bool) -> ToolResult<Shown>;
    /// A short notice in the app; never contains a secret or a result row.
    async fn toast(&self, text: &str);
}
