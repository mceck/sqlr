//! Saved tasks (spec §8.5): kept in memory and in `sqlr.db`, run on demand through the
//! same jobs the wizards start, with every run recorded.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use parking_lot::Mutex;
use sqlr_core::{Endpoint, SavedTask, TaskSpec};

use crate::engine::{Engine, Shared};
use crate::job::{JobId, JobKind, JobOutcome, JobState};
use crate::library::now_ms;

#[derive(Default)]
struct State {
    /// Sorted by name, then id.
    list: Arc<Vec<SavedTask>>,
    /// Whether the startup load from `sqlr.db` has landed.
    loaded: bool,
    /// Tasks deleted before the startup load landed: their stored rows are gone.
    deleted_early: HashSet<String>,
}

#[derive(Default)]
pub(crate) struct Tasks {
    state: Mutex<State>,
}

fn sort(list: &mut [SavedTask]) {
    list.sort_by(|a, b| (a.name.to_lowercase(), &a.id).cmp(&(b.name.to_lowercase(), &b.id)));
}

impl Shared {
    /// Queues the startup load of the tasks; it runs before any later write.
    pub(crate) fn load_tasks(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.db.push(move |db| {
            let result = db.list_tasks();
            let Some(shared) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(stored) => {
                    {
                        let mut st = shared.tasks.state.lock();
                        let st = &mut *st;
                        st.loaded = true;
                        let deleted = std::mem::take(&mut st.deleted_early);
                        let list = Arc::make_mut(&mut st.list);
                        let known: HashSet<String> = list.iter().map(|t| t.id.clone()).collect();
                        list.extend(
                            stored
                                .into_iter()
                                .filter(|t| !known.contains(&t.id) && !deleted.contains(&t.id)),
                        );
                        sort(list);
                    }
                    shared.repaint();
                }
                Err(e) => shared.notice(format!("Could not load tasks: {e}")),
            }
        });
    }

    fn save_task(self: &Arc<Self>, task: SavedTask) {
        {
            let mut st = self.tasks.state.lock();
            let list = Arc::make_mut(&mut st.list);
            match list.iter_mut().find(|t| t.id == task.id) {
                Some(existing) => *existing = task.clone(),
                None => list.push(task.clone()),
            }
            sort(list);
            st.deleted_early.remove(&task.id);
        }
        let weak = Arc::downgrade(self);
        self.db.push(move |db| {
            if let Err(e) = db.save_task(&task)
                && let Some(shared) = weak.upgrade()
            {
                shared.notice(format!("Could not save the task: {e}"));
            }
        });
        self.repaint();
    }

    fn delete_task(self: &Arc<Self>, id: &str) {
        {
            let mut st = self.tasks.state.lock();
            Arc::make_mut(&mut st.list).retain(|t| t.id != id);
            if !st.loaded {
                st.deleted_early.insert(id.to_string());
            }
        }
        let id = id.to_string();
        let weak = Arc::downgrade(self);
        self.db.push(move |db| {
            if let Err(e) = db.delete_task(&id)
                && let Some(shared) = weak.upgrade()
            {
                shared.notice(format!("Could not delete the task: {e}"));
            }
        });
        self.repaint();
    }
}

/// How a finished job reads in a run's record: its outcome word and one summary line.
fn describe(outcome: &Option<JobOutcome>) -> (&'static str, String) {
    match outcome {
        Some(JobOutcome::Imported {
            read,
            written,
            rejected,
        }) => (
            "ok",
            format!(
                "Read {read} rows, wrote {written}, rejected {}",
                rejected.len()
            ),
        ),
        Some(JobOutcome::Exported { rows, path }) => {
            ("ok", format!("Wrote {rows} rows to {}", path.display()))
        }
        Some(JobOutcome::Dumped { path }) => ("ok", format!("Dumped to {}", path.display())),
        Some(JobOutcome::Restored { path }) => ("ok", format!("Restored from {}", path.display())),
        Some(JobOutcome::Failed(e)) => ("failed", e.message.clone()),
        Some(JobOutcome::Cancelled) => ("cancelled", "Cancelled".to_string()),
        _ => ("ok", "Done".to_string()),
    }
}

/// Why a saved task cannot start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TaskError {
    /// The task was deleted.
    #[error("This task no longer exists")]
    Gone,
    /// A connection the task uses is not connected.
    #[error("The connection \"{id}\" is not connected, or no longer exists")]
    NotConnected {
        /// The connection id.
        id: String,
    },
    /// An input file does not exist.
    #[error("The file {} does not exist", path.display())]
    FileMissing {
        /// The missing file.
        path: PathBuf,
    },
    /// The output's nearest existing ancestor is not a directory.
    #[error("{} is not a directory", path.display())]
    NotADirectory {
        /// The ancestor.
        path: PathBuf,
    },
    /// The output's directory is read-only.
    #[error("The directory {} is read-only", path.display())]
    ReadOnlyDirectory {
        /// The directory.
        path: PathBuf,
    },
    /// Script tasks are not implemented.
    #[error("Script tasks cannot run yet")]
    ScriptUnsupported,
}

/// Whether a file can be written at `path`: its nearest existing ancestor is a writable
/// directory.
fn writable_target(path: &Path) -> Result<(), TaskError> {
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d.as_os_str().is_empty() {
            return Ok(());
        }
        if let Ok(meta) = std::fs::metadata(d) {
            return if !meta.is_dir() {
                Err(TaskError::NotADirectory {
                    path: d.to_path_buf(),
                })
            } else if meta.permissions().readonly() {
                Err(TaskError::ReadOnlyDirectory {
                    path: d.to_path_buf(),
                })
            } else {
                Ok(())
            };
        }
        dir = d.parent();
    }
    Ok(())
}

impl Engine {
    /// Every saved task, sorted by name; empty until loaded at startup.
    pub fn tasks(&self) -> Arc<Vec<SavedTask>> {
        self.shared.tasks.state.lock().list.clone()
    }

    /// Inserts or replaces (by id) a task: in memory at once, in `sqlr.db` behind.
    pub fn save_task(&self, task: SavedTask) {
        self.shared.save_task(task);
    }

    /// Deletes a task and its runs.
    pub fn delete_task(&self, id: &str) {
        self.shared.delete_task(id);
    }

    /// Checks what a run needs before it starts: connected connections, input files and a
    /// writable place for output. The message names what is wrong.
    fn validate_task(&self, task: &SavedTask) -> Result<(), TaskError> {
        for id in task.spec.connections() {
            if self.shared.live_io(&id).is_none() {
                return Err(TaskError::NotConnected { id });
            }
        }
        let exists = |path: &Path| {
            if path.exists() {
                Ok(())
            } else {
                Err(TaskError::FileMissing {
                    path: path.to_path_buf(),
                })
            }
        };
        match &task.spec {
            TaskSpec::Restore { input, .. } => exists(input)?,
            TaskSpec::Dump { out, .. } => writable_target(out)?,
            TaskSpec::Transfer(spec) => {
                if let Endpoint::File { path, .. }
                | Endpoint::Workbook { path, .. }
                | Endpoint::Parquet { path } = &spec.source
                {
                    exists(path)?;
                }
                if let Endpoint::File { path, .. }
                | Endpoint::Workbook { path, .. }
                | Endpoint::Parquet { path } = &spec.target
                {
                    writable_target(path)?;
                }
            }
            TaskSpec::Script { .. } => {}
        }
        Ok(())
    }

    /// Runs a saved task as the job its spec stands for, and records the run (start, end,
    /// outcome, summary and log). `Err` is the reason it could not start; nothing is
    /// recorded then.
    pub fn run_task(&self, id: &str) -> Result<JobId, TaskError> {
        let task = self
            .tasks()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .ok_or(TaskError::Gone)?;
        self.validate_task(&task)?;
        let job = match &task.spec {
            TaskSpec::Dump {
                conn,
                database,
                options,
                out,
            } => self.run_dump(conn, database, options.clone(), out.clone()),
            TaskSpec::Restore {
                conn,
                database,
                options,
                input,
            } => self.run_restore(conn, database, options.clone(), input.clone()),
            TaskSpec::Transfer(spec) => self.transfer(spec.clone()),
            TaskSpec::Script { .. } => {
                return Err(TaskError::ScriptUnsupported);
            }
        };
        let started = now_ms();
        let run_id = Arc::new(AtomicI64::new(-1));
        let task_id = task.id;
        let started_run = run_id.clone();
        self.shared
            .db
            .push(move |db| match db.add_run(&task_id, started) {
                Ok(id) => started_run.store(id, Ordering::SeqCst),
                Err(e) => tracing::warn!("could not record a task run: {e}"),
            });
        let shared = self.shared.clone();
        self.shared.jobs.observe(job, move |state: JobState| {
            let (word, summary) = describe(&state.outcome);
            let log = state.log.join("\n");
            let queue = shared.clone();
            shared.db.push(move |db| {
                let id = run_id.load(Ordering::SeqCst);
                if id >= 0
                    && let Err(e) = db.finish_run(id, now_ms(), word, &summary, &log)
                {
                    tracing::warn!("could not finish a task run: {e}");
                }
            });
            queue.repaint();
        });
        Ok(job)
    }

    /// Loads a task's runs, newest first, as a job ([`JobOutcome::TaskRuns`]).
    pub fn task_runs(&self, id: &str) -> JobId {
        let job = self.shared.jobs.create(JobKind::Preview);
        let job_id = job.id;
        let task_id = id.to_string();
        let shared = self.shared.clone();
        self.shared.db.push(move |db| {
            let outcome = match db.runs(&task_id, sqlr_store::RUNS_KEPT) {
                Ok(runs) => JobOutcome::TaskRuns(runs),
                Err(e) => JobOutcome::Failed(sqlr_core::DriverError::new(
                    sqlr_core::ErrorKind::Other,
                    e.to_string(),
                )),
            };
            shared.jobs.finish(job_id, outcome);
            shared.repaint();
        });
        job_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_errors_keep_their_messages() {
        assert_eq!(
            TaskError::NotConnected { id: "pg".into() }.to_string(),
            "The connection \"pg\" is not connected, or no longer exists"
        );
        assert_eq!(
            TaskError::FileMissing {
                path: PathBuf::from("in.csv")
            }
            .to_string(),
            "The file in.csv does not exist"
        );
    }
}
