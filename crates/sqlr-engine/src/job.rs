//! Background jobs (applying edits, export, import, file previews, large copies, object
//! definitions and DDL applies): their progress and outcome as the UI sees them, and their
//! cancellation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use sqlr_core::{CancelHandle, DriverError, ErrorKind, ObjectDef, Value};

use crate::admin::AdminSnapshot;
use crate::library::now_ms;
use crate::objects::DdlReport;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Apply,
    Export,
    Import,
    Preview,
    Clipboard,
    /// Loading an object definition (`Engine::load_object`).
    LoadObject,
    /// Writing an explorer node's `CREATE` statement (`Engine::load_ddl`).
    LoadDdl,
    /// Applying DDL (`Engine::apply_ddl`).
    ApplyDdl,
    /// Reading roles, grants, sessions, locks, properties or statistics
    /// (`Engine::admin_snapshot`).
    AdminSnapshot,
    /// Cancelling or terminating a server session (`Engine::admin_action`).
    AdminAction,
    /// A Redis `SCAN` (`Engine::kv_scan`).
    KvScan,
    /// Loading a Redis key or its next page (`Engine::kv_open`, `kv_load_more`).
    KvValue,
    /// Editing, creating, renaming or deleting Redis keys (`Engine::kv_edit`, `kv_delete`).
    KvEdit,
    /// A Redis console command (`Engine::kv_command`).
    KvConsole,
    /// Saving, inserting or deleting documents (`Engine::save_doc`, `delete_docs`).
    DocEdit,
    /// A document statement run as a job: index administration (`Engine::doc_admin`).
    DocAdmin,
    /// Comparing two schemas (`Engine::compare_schemas`).
    Compare,
    /// The plan of a statement (`Engine::explain`).
    Explain,
    /// Reading a schema for an ER diagram (`Engine::er_model`).
    ErModel,
    /// A native database dump (`Engine::run_dump`).
    Dump,
    /// A native database restore (`Engine::run_restore`).
    Restore,
}

/// How a changeset ended. On a failure nothing stays applied: `applied` is 0 and `failed`
/// names the row whose statement failed or did not affect exactly one row.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplyReport {
    pub applied: usize,
    pub failed: Option<(String, DriverError)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobProgress {
    pub rows: u64,
    pub bytes: u64,
}

/// What looking into a transfer source found: its columns with their types, the first rows
/// (at most [`crate::PREVIEW_ROWS`]) and, for a workbook, the names of its sheets.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceProbe {
    pub columns: Vec<sqlr_core::ColumnMeta>,
    pub rows: Vec<Vec<Value>>,
    pub sheets: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobOutcome {
    Applied(ApplyReport),
    Exported {
        rows: u64,
        path: PathBuf,
    },
    /// `rejected` lists the skipped rows by their line in the file (the object's position
    /// for a JSON array), with the reason; only the first 1000 are kept.
    Imported {
        read: u64,
        written: u64,
        rejected: Vec<(u64, String)>,
    },
    /// The first rows of a file, for the import dialog.
    Preview {
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
    /// Text built for the clipboard.
    Copied(String),
    /// Typed, masked cells and their interoperable text representation.
    GridCopied(Arc<sqlr_core::GridClipboard>),
    /// The tables and foreign keys of a schema (`Engine::er_model`).
    ErModel(sqlr_core::ErModel),
    /// A saved ER layout, if any (`Engine::load_er_layout`).
    ErLayout(Option<String>),
    /// A statement's plan (`Engine::explain`).
    Plan(sqlr_core::PlanTree),
    /// The differences of a schema compare, by object (`Engine::compare_schemas`).
    Compared(Vec<sqlr_core::ObjectDiff>),
    /// A transfer source's columns and first rows (`Engine::probe_source`).
    Probed(SourceProbe),
    /// An object definition, loaded.
    Object(ObjectDef),
    /// The DDL that creates an explorer node's object.
    Ddl(String),
    /// DDL ran (fully or not: see [`DdlReport::ok`]).
    DdlApplied(DdlReport),
    /// What an administration snapshot read.
    Admin(AdminSnapshot),
    /// The job ran and has nothing more to report: an administration action, an index
    /// administration statement (`Engine::doc_admin`), or a
    /// key-value request whose result is in the connection's key-value state
    /// (`Engine::kv_keys`, `kv_value`, `kv_console`).
    Done,
    /// Documents saved, inserted or deleted: `written` of the `requested` ones (a document
    /// deleted meanwhile is not written); `id` is an inserted document's `_id`.
    DocsWritten {
        requested: u64,
        written: u64,
        id: Option<sqlr_core::DocValue>,
    },
    /// A native dump finished and wrote `path` (a file, or a directory for PostgreSQL's
    /// directory format).
    Dumped {
        path: PathBuf,
    },
    /// A native restore finished from `path`.
    Restored {
        path: PathBuf,
    },
    Failed(DriverError),
    Cancelled,
}

impl JobOutcome {
    fn failure(&self) -> Option<&DriverError> {
        match self {
            Self::Failed(error) => Some(error),
            Self::Applied(report) => report.failed.as_ref().map(|(_, error)| error),
            Self::DdlApplied(report) => report.failure(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct JobState {
    pub kind: JobKind,
    pub progress: JobProgress,
    /// `None` while the job runs.
    pub outcome: Option<JobOutcome>,
    /// When the job started, in Unix milliseconds.
    pub started_ms: i64,
    /// The job's log, oldest first (native tools write their output here). Bounded.
    pub log: Vec<String>,
    /// What `progress` counts toward, when the job knows (SQLite backup: pages).
    pub total: Option<u64>,
}

/// The job log keeps this many lines.
const MAX_LOG_LINES: usize = 5_000;

/// What a running job checks to stop early.
#[derive(Clone)]
pub(crate) struct JobControl {
    pub id: JobId,
    cancelled: Arc<AtomicBool>,
    jobs: Arc<JobsInner>,
}

impl JobControl {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn progress(&self, progress: JobProgress) {
        if let Some(entry) = self.jobs.map.lock().get_mut(&self.id) {
            entry.state.progress = progress;
            if let (Some(ops), Some(id)) = (&self.jobs.operations, entry.operation) {
                ops.progress(id, progress);
            }
        }
    }

    /// Declares what the job's progress counts toward.
    pub fn set_total(&self, total: u64) {
        if let Some(entry) = self.jobs.map.lock().get_mut(&self.id) {
            entry.state.total = Some(total);
        }
    }

    /// The statement a cancel interrupts, while the job runs one.
    pub fn set_cancel(&self, handle: Option<Arc<dyn CancelHandle>>) {
        if let Some(entry) = self.jobs.map.lock().get_mut(&self.id) {
            entry.cancel = handle;
        }
    }

    /// Appends one line to the job's log, dropping the oldest past [`MAX_LOG_LINES`].
    pub fn log(&self, line: impl Into<String>) {
        if let Some(entry) = self.jobs.map.lock().get_mut(&self.id) {
            if entry.state.log.len() >= MAX_LOG_LINES {
                entry.state.log.remove(0);
            }
            entry.state.log.push(line.into());
        }
    }
}

struct JobEntry {
    state: JobState,
    cancelled: Arc<AtomicBool>,
    cancel: Option<Arc<dyn CancelHandle>>,
    operation: Option<crate::OperationId>,
    /// The command task that created it, when it was created there.
    run: Option<u64>,
}

#[derive(Default)]
struct JobsInner {
    map: Mutex<HashMap<JobId, JobEntry>>,
    next: Mutex<u64>,
    operations: Option<crate::operations::Operations>,
}

#[derive(Default)]
pub(crate) struct Jobs {
    inner: Arc<JobsInner>,
}

impl Jobs {
    pub fn with_operations(operations: crate::operations::Operations) -> Self {
        Self {
            inner: Arc::new(JobsInner {
                operations: Some(operations),
                ..Default::default()
            }),
        }
    }

    pub fn create(&self, kind: JobKind) -> JobControl {
        let id = {
            let mut next = self.inner.next.lock();
            *next += 1;
            JobId(*next)
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        self.inner.map.lock().insert(
            id,
            JobEntry {
                state: JobState {
                    kind,
                    progress: JobProgress::default(),
                    outcome: None,
                    started_ms: now_ms(),
                    log: Vec::new(),
                    total: None,
                },
                cancelled: cancelled.clone(),
                cancel: None,
                operation: self.inner.operations.as_ref().map(|ops| {
                    ops.start(
                        crate::OperationKind::Job(kind),
                        format!("{kind:?}"),
                        None,
                        crate::operations::Target::Job(id),
                    )
                }),
                run: crate::operations::current_run(),
            },
        );
        JobControl {
            id,
            cancelled,
            jobs: self.inner.clone(),
        }
    }

    pub fn bind(&self, id: JobId, run: u64, conn: Option<String>) {
        if let Some(entry) = self.inner.map.lock().get_mut(&id) {
            entry.run = Some(run);
            if let (Some(ops), Some(operation)) = (&self.inner.operations, entry.operation) {
                ops.bind(operation, run, conn);
            }
        }
    }

    pub fn operation(&self, id: JobId) -> Option<crate::OperationId> {
        self.inner
            .map
            .lock()
            .get(&id)
            .and_then(|entry| entry.operation)
    }

    pub fn state(&self, id: JobId) -> Option<JobState> {
        self.inner.map.lock().get(&id).map(|e| e.state.clone())
    }

    pub fn finish(&self, id: JobId, outcome: JobOutcome) {
        {
            let mut map = self.inner.map.lock();
            match map.get_mut(&id) {
                Some(entry) if entry.state.outcome.is_none() => {
                    if let (Some(ops), Some(id)) = (&self.inner.operations, entry.operation) {
                        let state = match &outcome {
                            JobOutcome::Cancelled => crate::OperationState::Cancelled,
                            JobOutcome::Failed(e) => crate::OperationState::Failed(e.to_string()),
                            _ => crate::OperationState::Done {
                                ok: outcome.failure().is_none(),
                            },
                        };
                        ops.progress(id, entry.state.progress);
                        ops.finish(id, state);
                    }
                    log_job_failure(id, entry.operation, &entry.state, &outcome);
                    entry.state.outcome = Some(outcome);
                    entry.cancel = None;
                }
                _ => {}
            }
        }
    }

    /// Fails every unfinished job created by command task `run` (a panic in that task), and
    /// returns their ids and kinds for the incident.
    pub fn fail_run(&self, run: u64, message: &str) -> Vec<(JobId, JobKind)> {
        let ids: Vec<(JobId, JobKind)> = {
            let map = self.inner.map.lock();
            map.iter()
                .filter(|(_, entry)| entry.run == Some(run) && entry.state.outcome.is_none())
                .map(|(id, entry)| (*id, entry.state.kind))
                .collect()
        };
        for (id, _) in &ids {
            self.finish(
                *id,
                JobOutcome::Failed(DriverError::new(ErrorKind::Other, message)),
            );
        }
        ids
    }

    /// Flags the job cancelled and returns the statement to interrupt, if it runs one.
    pub fn cancel(&self, id: JobId) -> Option<Arc<dyn CancelHandle>> {
        let map = self.inner.map.lock();
        let entry = map.get(&id)?;
        if entry.state.outcome.is_some() {
            return None;
        }
        entry.cancelled.store(true, Ordering::SeqCst);
        entry.cancel.clone()
    }

    pub fn forget(&self, id: JobId) {
        self.inner.map.lock().remove(&id);
    }
}

/// One failure line when a job settles. Nothing for a success: the UI already shows it.
fn log_job_failure(
    id: JobId,
    operation: Option<crate::OperationId>,
    state: &JobState,
    outcome: &JobOutcome,
) {
    let elapsed_ms = (now_ms() - state.started_ms).max(0) as u64;
    match outcome.failure() {
        Some(e) => {
            let level = crate::log::failure_level(e.kind);
            crate::log::log_at!(
                level,
                target: "sqlr_engine::operation",
                op = "job",
                job_id = id.0,
                operation_id = operation.map(|id| id.0),
                job = ?state.kind,
                elapsed_ms,
                error_kind = crate::log::kind_name(e.kind),
                error = %e.message,
                "job failed"
            );
        }
        None if matches!(outcome, JobOutcome::Cancelled) => {
            tracing::debug!(
                target: "sqlr_engine::operation",
                op = "job",
                job_id = id.0,
                operation_id = operation.map(|id| id.0),
                job = ?state.kind,
                elapsed_ms,
                error_kind = "Cancelled",
                "job cancelled"
            );
        }
        _ => {}
    }
}
