use rusqlite::params;
use sqlr_core::library::{HistoryEntry, HistoryOutcome, HistoryQuery};

use crate::StoreError;
use crate::db::{Db, MAX_HISTORY_SQL_BYTES, db_err};

fn truncate_at_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl Db {
    /// Stores `e` (its `id` is ignored) and returns the new id.
    pub fn add_history(&self, e: &HistoryEntry) -> Result<i64, StoreError> {
        let sql = truncate_at_char_boundary(&e.sql, MAX_HISTORY_SQL_BYTES);
        let (outcome, count, error): (&str, Option<i64>, Option<&str>) = match &e.outcome {
            HistoryOutcome::Rows(n) => ("rows", Some(*n as i64), None),
            HistoryOutcome::Affected(n) => ("affected", Some(*n as i64), None),
            HistoryOutcome::Failed(m) => ("failed", None, Some(m)),
            HistoryOutcome::Cancelled => ("cancelled", None, None),
        };
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO history (conn_id, database, sql, started_at, duration_ms, outcome, count, error) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                e.conn_id,
                e.database,
                sql,
                e.started_at_ms,
                e.duration_ms as i64,
                outcome,
                count,
                error
            ],
        )
        .map_err(db_err)?;
        Ok(conn.last_insert_rowid())
    }

    /// Newest first.
    pub fn search_history(&self, q: &HistoryQuery) -> Result<Vec<HistoryEntry>, StoreError> {
        let limit = if q.limit == 0 { 200 } else { q.limit };
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, conn_id, database, sql, started_at, duration_ms, outcome, count, error \
                 FROM history \
                 WHERE sql LIKE '%' || ?1 || '%' ESCAPE '\\' AND (?2 IS NULL OR conn_id = ?2) \
                 ORDER BY started_at DESC, id DESC LIMIT ?3",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(
                params![escape_like(&q.text), q.conn_id, limit as i64],
                |r| {
                    let outcome: String = r.get(6)?;
                    let count: Option<i64> = r.get(7)?;
                    let error: Option<String> = r.get(8)?;
                    let count = count.unwrap_or(0).max(0) as u64;
                    let outcome = match outcome.as_str() {
                        "rows" => HistoryOutcome::Rows(count),
                        "affected" => HistoryOutcome::Affected(count),
                        "failed" => HistoryOutcome::Failed(error.unwrap_or_default()),
                        _ => HistoryOutcome::Cancelled,
                    };
                    Ok(HistoryEntry {
                        id: r.get(0)?,
                        conn_id: r.get(1)?,
                        database: r.get(2)?,
                        sql: r.get(3)?,
                        started_at_ms: r.get(4)?,
                        duration_ms: r.get::<_, i64>(5)?.max(0) as u64,
                        outcome,
                    })
                },
            )
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }

    /// Deletes the connection's history entries, or every connection's with `None`.
    pub fn clear_history(&self, conn_id: Option<&str>) -> Result<(), StoreError> {
        let conn = self.conn.lock();
        match conn_id {
            Some(id) => conn.execute("DELETE FROM history WHERE conn_id = ?1", params![id]),
            None => conn.execute("DELETE FROM history", []),
        }
        .map_err(db_err)?;
        Ok(())
    }
}
