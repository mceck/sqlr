//! The engine: database sessions (one per editor tab plus one for the explorer), their
//! transactions and SSH tunnels, explorer and catalog caches, paged query results, and the
//! query history and snippets, driven by a tokio runtime off the UI thread. Every public
//! method returns immediately; the UI reads shared state each frame and is told to repaint
//! on every change.

mod admin;
mod apply;
mod catalog;
mod compare;
mod docs;
mod drivers;
mod engine;
mod explain;
mod job;
mod kv;
mod library;
mod objects;
mod operations;
pub use operations::{OperationId, OperationInfo, OperationKind, OperationState};
mod registry;
mod result;
mod script;
mod session;
mod ssh;
mod store;
mod tasks;
mod tools;
mod transfer;
mod tx;

pub use admin::{AdminAction, AdminQuery, AdminSnapshot};
pub use catalog::ConnCatalog;
pub use compare::SchemaSide;
pub use docs::{DOC_RESULT_BUDGET, DocResult};
pub use drivers::{DriverJobState, IndexState, ManualState};
pub use engine::{ConnectionSettings, Engine, Repaint};
pub use job::{ApplyReport, JobId, JobKind, JobOutcome, JobProgress, JobState, SourceProbe};
pub use kv::{ConsoleSnapshot, KvKeys, KvValue};
pub use library::{HistoryChannel, HistoryView};
pub use objects::{ApplyOptions, DdlReport, DdlStepOutcome, Masking};
pub use registry::DriverRegistry;
pub use result::{QueryId, QueryResult, QueryStatus};
pub use script::{OutputLine, ScriptId, ScriptOnError, ScriptState};
pub use session::{LoadState, SessionKey, SessionStatus, Slot};
pub use sqlr_registry::{InstalledDriver, ManualInspection, RegistryEntry, is_newer};
pub use ssh::{ConnectReport, HostKeyInfo};
pub use transfer::{ExportSource, ImportOptions, PREVIEW_ROWS};
pub use tx::{MessageLevel, RunMessage, TxPhase, TxState};
