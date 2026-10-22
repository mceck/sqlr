//! Saved tasks (spec §8.5): a dump, restore, transfer or script kept under a name and run
//! again on demand. A task names its connections by id and holds no secret.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::tools::{DumpOptions, RestoreOptions};
use crate::transfer::TransferSpec;

/// What a task does. Tasks are few and short-lived values, so the variants are not boxed.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TaskSpec {
    Dump {
        conn: String,
        database: String,
        options: DumpOptions,
        out: PathBuf,
    },
    Restore {
        conn: String,
        database: String,
        options: RestoreOptions,
        input: PathBuf,
    },
    Transfer(TransferSpec),
    Script {
        conn: String,
        database: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTask {
    pub id: String,
    pub name: String,
    pub spec: TaskSpec,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// One execution of a task.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskRun {
    pub id: i64,
    pub task_id: String,
    pub started_ms: i64,
    /// `None` while the run is going (or the app closed before it ended).
    pub ended_ms: Option<i64>,
    /// `ok`, `failed`, `cancelled` or `running`.
    pub outcome: String,
    pub summary: String,
    pub log: String,
}
