//! Saved tasks and their run history (spec §8.5), in `sqlr.db`.

use rusqlite::{OptionalExtension, params};
use sqlr_core::{SavedTask, TaskRun, TaskSpec};

use crate::StoreError;
use crate::db::{Db, db_err};

/// Runs kept per task: the newest.
pub const RUNS_KEPT: usize = 50;
/// A run's log is cut to its last this many bytes.
pub const MAX_RUN_LOG_BYTES: usize = 1 << 20;

impl Db {
    /// Every task, sorted by name.
    pub fn list_tasks(&self) -> Result<Vec<SavedTask>, StoreError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, name, spec_json, created_ms, updated_ms FROM tasks \
                 ORDER BY name COLLATE NOCASE, id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .map_err(db_err)?;
        let mut tasks = Vec::new();
        for row in rows {
            let (id, name, json, created_ms, updated_ms) = row.map_err(db_err)?;
            // A task this version cannot read (written by a newer one) is skipped, not fatal.
            if let Ok(spec) = serde_json::from_str::<TaskSpec>(&json) {
                tasks.push(SavedTask {
                    id,
                    name,
                    spec,
                    created_ms,
                    updated_ms,
                });
            }
        }
        Ok(tasks)
    }

    /// Inserts, or replaces the task with the same id.
    pub fn save_task(&self, task: &SavedTask) -> Result<(), StoreError> {
        let json = serde_json::to_string(&task.spec).map_err(db_err)?;
        self.conn
            .lock()
            .execute(
                "INSERT INTO tasks (id, name, kind, spec_json, created_ms, updated_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, kind = excluded.kind, \
                 spec_json = excluded.spec_json, updated_ms = excluded.updated_ms",
                params![
                    task.id,
                    task.name,
                    task.spec.kind(),
                    json,
                    task.created_ms,
                    task.updated_ms
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// The stored JSON of a task's spec (what is on disk).
    #[doc(hidden)]
    pub fn task_spec_json(&self, id: &str) -> Result<Option<String>, StoreError> {
        self.conn
            .lock()
            .query_row("SELECT spec_json FROM tasks WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(db_err)
    }

    /// Deletes a task and its runs.
    pub fn delete_task(&self, id: &str) -> Result<(), StoreError> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM task_runs WHERE task_id = ?1", [id])
            .map_err(db_err)?;
        conn.execute("DELETE FROM tasks WHERE id = ?1", [id])
            .map_err(db_err)?;
        Ok(())
    }

    /// Starts a run and returns its id; only the newest [`RUNS_KEPT`] runs of the task stay.
    pub fn add_run(&self, task_id: &str, started_ms: i64) -> Result<i64, StoreError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO task_runs (task_id, started_ms, outcome, summary, log) \
             VALUES (?1, ?2, 'running', '', '')",
            params![task_id, started_ms],
        )
        .map_err(db_err)?;
        let id = conn.last_insert_rowid();
        conn.execute(
            "DELETE FROM task_runs WHERE task_id = ?1 AND id NOT IN \
             (SELECT id FROM task_runs WHERE task_id = ?1 \
              ORDER BY started_ms DESC, id DESC LIMIT ?2)",
            params![task_id, RUNS_KEPT as i64],
        )
        .map_err(db_err)?;
        Ok(id)
    }

    /// Ends a run with its outcome, summary and log (cut to its last 1 MiB).
    pub fn finish_run(
        &self,
        run_id: i64,
        ended_ms: i64,
        outcome: &str,
        summary: &str,
        log: &str,
    ) -> Result<(), StoreError> {
        let log = if log.len() > MAX_RUN_LOG_BYTES {
            let mut start = log.len() - MAX_RUN_LOG_BYTES;
            while !log.is_char_boundary(start) {
                start += 1;
            }
            &log[start..]
        } else {
            log
        };
        self.conn
            .lock()
            .execute(
                "UPDATE task_runs SET ended_ms = ?2, outcome = ?3, summary = ?4, log = ?5 \
                 WHERE id = ?1",
                params![run_id, ended_ms, outcome, summary, log],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// The newest `limit` runs of a task, newest first.
    pub fn runs(&self, task_id: &str, limit: usize) -> Result<Vec<TaskRun>, StoreError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, task_id, started_ms, ended_ms, outcome, summary, log FROM task_runs \
                 WHERE task_id = ?1 ORDER BY started_ms DESC, id DESC LIMIT ?2",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![task_id, limit as i64], |r| {
                Ok(TaskRun {
                    id: r.get(0)?,
                    task_id: r.get(1)?,
                    started_ms: r.get(2)?,
                    ended_ms: r.get(3)?,
                    outcome: r.get(4)?,
                    summary: r.get(5)?,
                    log: r.get(6)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }
}
