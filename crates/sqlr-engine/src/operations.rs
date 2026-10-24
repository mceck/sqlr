//! In-flight work, its cancellation path and bounded finished retention.
use crate::{Engine, JobId, JobKind, JobProgress, QueryId};
use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Notify;

tokio::task_local! {
    /// The command task currently running, so an operation it starts can be failed if the
    /// task panics.
    pub static RUN: u64;
}

/// The current command task's id, when called inside one.
pub(crate) fn current_run() -> Option<u64> {
    RUN.try_with(|run| *run).ok()
}

/// A future that marks every poll as belonging to an engine command task, so the global
/// panic hook leaves the incident to the engine converter.
pub(crate) struct EngineTask<F>(pub(crate) F);

impl<F: std::future::Future> std::future::Future for EngineTask<F> {
    type Output = F::Output;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let _scope = sqlr_core::panic_scope::EngineTaskScope::enter();
        // SAFETY: `EngineTask` never moves or drops the inner future itself; this is the
        // standard pin projection and the inner value is only ever used through `Pin`.
        let inner = unsafe { self.map_unchecked_mut(|task| &mut task.0) };
        inner.poll(cx)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OperationId(pub u64);
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationKind {
    Connect,
    Browse,
    Query,
    Fetch,
    Script,
    Job(JobKind),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationState {
    Running,
    Cancelling,
    Done { ok: bool },
    Cancelled,
    Failed(String),
}
#[derive(Clone, Debug)]
pub struct OperationInfo {
    pub id: OperationId,
    pub kind: OperationKind,
    pub label: String,
    pub conn: Option<String>,
    pub started_ms: i64,
    pub state: OperationState,
    pub progress: JobProgress,
    pub cancelable: bool,
    pub elapsed: Duration,
}
#[derive(Clone)]
pub(crate) enum Target {
    Connect(Arc<Notify>),
    Read(Arc<Notify>),
    Query(QueryId),
    Job(JobId),
    Script(crate::ScriptId),
}
struct Entry {
    info: OperationInfo,
    target: Target,
    start: Instant,
    finished: Option<Instant>,
    /// The command task that started it, if any.
    run: Option<u64>,
}
struct Inner {
    next: u64,
    entries: BTreeMap<OperationId, Entry>,
    retention: Duration,
}
#[derive(Clone)]
pub(crate) struct Operations(Arc<Mutex<Inner>>);
impl Default for Operations {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Inner {
            next: 0,
            entries: BTreeMap::new(),
            retention: Duration::from_secs(30),
        })))
    }
}
impl Operations {
    pub fn start(
        &self,
        kind: OperationKind,
        label: String,
        conn: Option<String>,
        target: Target,
    ) -> OperationId {
        let mut inner = self.0.lock();
        inner.next += 1;
        let id = OperationId(inner.next);
        let cancelable = true;
        inner.entries.insert(
            id,
            Entry {
                info: OperationInfo {
                    id,
                    kind,
                    label,
                    conn,
                    started_ms: crate::library::now_ms(),
                    state: OperationState::Running,
                    progress: JobProgress::default(),
                    cancelable,
                    elapsed: Duration::ZERO,
                },
                target,
                start: Instant::now(),
                finished: None,
                run: current_run(),
            },
        );
        tracing::Span::current().record("operation_id", id.0);
        id
    }

    pub fn bind(&self, id: OperationId, run: u64, conn: Option<String>) {
        if let Some(entry) = self.0.lock().entries.get_mut(&id) {
            entry.run = Some(run);
            entry.info.conn = conn;
        }
    }

    /// Fails every still-running operation started by command task `run`, and returns them
    /// so the caller can name them in an incident.
    pub fn fail_run(&self, run: u64, message: &str) -> Vec<OperationInfo> {
        let mut inner = self.0.lock();
        let now = Instant::now();
        let mut failed = Vec::new();
        for entry in inner.entries.values_mut() {
            if entry.run != Some(run)
                || !matches!(
                    entry.info.state,
                    OperationState::Running | OperationState::Cancelling
                )
            {
                continue;
            }
            entry.info.state = OperationState::Failed(message.to_string());
            entry.info.cancelable = false;
            entry.info.elapsed = now.duration_since(entry.start);
            entry.finished = Some(now);
            failed.push(entry.info.clone());
        }
        failed
    }
    pub fn finish(&self, id: OperationId, state: OperationState) {
        if let Some(e) = self.0.lock().entries.get_mut(&id)
            && e.finished.is_none()
        {
            e.info.state = state;
            e.info.cancelable = false;
            e.finished = Some(Instant::now());
        }
    }
    pub fn progress(&self, id: OperationId, progress: JobProgress) {
        if let Some(e) = self.0.lock().entries.get_mut(&id) {
            e.info.progress = progress;
        }
    }
    pub fn cancel(&self, id: OperationId) -> Option<Target> {
        let mut inner = self.0.lock();
        let e = inner.entries.get_mut(&id)?;
        if !e.info.cancelable || e.finished.is_some() || e.info.state == OperationState::Cancelling
        {
            return None;
        }
        e.info.state = OperationState::Cancelling;
        Some(e.target.clone())
    }
    pub fn list(&self) -> Vec<OperationInfo> {
        let mut inner = self.0.lock();
        let retention = inner.retention;
        inner
            .entries
            .retain(|_, e| e.finished.is_none_or(|end| end.elapsed() < retention));
        let mut out: Vec<_> = inner
            .entries
            .values()
            .map(|e| {
                let mut i = e.info.clone();
                i.elapsed = e
                    .finished
                    .unwrap_or_else(Instant::now)
                    .duration_since(e.start);
                i
            })
            .collect();
        out.sort_by_key(|i| {
            (
                !matches!(
                    i.state,
                    OperationState::Running | OperationState::Cancelling
                ),
                std::cmp::Reverse(i.id),
            )
        });
        out
    }
}
impl Engine {
    pub fn operations(&self) -> Vec<OperationInfo> {
        self.shared.operations.list()
    }
    pub fn set_operations_retention(&self, seconds: u64) {
        self.shared.operations.0.lock().retention = Duration::from_secs(seconds);
    }
    pub fn forget_operation(&self, id: OperationId) {
        let mut inner = self.shared.operations.0.lock();
        if inner.entries.get(&id).is_some_and(|e| e.finished.is_some()) {
            inner.entries.remove(&id);
        }
    }
    pub fn cancel_operation(&self, id: OperationId) -> bool {
        let Some(target) = self.shared.operations.cancel(id) else {
            return false;
        };
        match target {
            Target::Connect(n) | Target::Read(n) => n.notify_one(),
            Target::Query(q) => self.cancel(q),
            Target::Job(j) => self.cancel_job(j),
            Target::Script(s) => self.cancel_script(s),
        };
        self.shared.repaint();
        true
    }
}
