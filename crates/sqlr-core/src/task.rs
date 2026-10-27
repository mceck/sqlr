//! Saved tasks (spec §8.5): a dump, restore, transfer or script kept under a name and run
//! again on demand. A task names its connections by id and holds no secret.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::tools::{DumpOptions, RestoreOptions};
use crate::transfer::TransferSpec;

/// What a task does. Tasks are few and short-lived values, so the variants are not boxed.
#[expect(
    clippy::large_enum_variant,
    reason = "tasks are few, short-lived values; boxing the large variants would only add indirection"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TaskSpec {
    /// Dumps a database to a file.
    Dump {
        /// The id of the connection.
        conn: String,
        /// The database to dump.
        database: String,
        /// What the dump contains and how it is written.
        options: DumpOptions,
        /// The dump file.
        out: PathBuf,
    },
    /// Restores a database from a dump file.
    Restore {
        /// The id of the connection.
        conn: String,
        /// The database to restore into.
        database: String,
        /// How the restore runs.
        options: RestoreOptions,
        /// The dump file to read.
        input: PathBuf,
    },
    /// Copies data between two connections or files.
    Transfer(TransferSpec),
    /// Runs a script on a connection.
    Script {
        /// The id of the connection.
        conn: String,
        /// The database the script runs in; the connection's default when `None`.
        database: Option<String>,
        /// The script text.
        sql: String,
    },
}

impl TaskSpec {
    /// The kind a task list groups by.
    pub fn kind(&self) -> &'static str {
        match self {
            TaskSpec::Dump { .. } => "dump",
            TaskSpec::Restore { .. } => "restore",
            TaskSpec::Transfer(_) => "transfer",
            TaskSpec::Script { .. } => "script",
        }
    }

    /// The ids of the connections the task uses.
    pub fn connections(&self) -> Vec<String> {
        match self {
            TaskSpec::Dump { conn, .. }
            | TaskSpec::Restore { conn, .. }
            | TaskSpec::Script { conn, .. } => vec![conn.clone()],
            TaskSpec::Transfer(spec) => {
                let mut ids: Vec<String> = Vec::new();
                for id in [spec.source.conn(), spec.target.conn()]
                    .into_iter()
                    .flatten()
                {
                    if !ids.iter().any(|i| i == id) {
                        ids.push(id.to_string());
                    }
                }
                ids
            }
        }
    }
}

/// A task kept under a name, to run again on demand.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTask {
    /// The task's stable id.
    pub id: String,
    /// The name the user gave it.
    pub name: String,
    /// What the task does.
    pub spec: TaskSpec,
    /// Creation time, in milliseconds since the Unix epoch.
    pub created_ms: i64,
    /// Time of the last change, in milliseconds since the Unix epoch.
    pub updated_ms: i64,
}

/// One execution of a task.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskRun {
    /// The run's id.
    pub id: i64,
    /// The id of the task that ran.
    pub task_id: String,
    /// Start time, in milliseconds since the Unix epoch.
    pub started_ms: i64,
    /// `None` while the run is going (or the app closed before it ended).
    pub ended_ms: Option<i64>,
    /// `ok`, `failed`, `cancelled` or `running`.
    pub outcome: String,
    /// A one-line result for the task list.
    pub summary: String,
    /// The full text the run logged.
    pub log: String,
}
