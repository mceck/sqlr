//! Recently used connections (the welcome screen's "Recent"): when each saved connection
//! last connected. Only ids and times are kept.

use rusqlite::params;

use crate::StoreError;
use crate::db::{Db, db_err};

impl Db {
    /// Records that `conn_id` connected at `at_ms` (Unix milliseconds).
    pub fn touch_recent(&self, conn_id: &str, at_ms: i64) -> Result<(), StoreError> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO recent_connections (conn_id, used_at_ms) VALUES (?1, ?2) \
                 ON CONFLICT(conn_id) DO UPDATE SET used_at_ms = excluded.used_at_ms",
                params![conn_id, at_ms],
            )
            .map(drop)
            .map_err(db_err)
    }

    /// Up to `limit` connections, most recently used first.
    pub fn recent(&self, limit: usize) -> Result<Vec<(String, i64)>, StoreError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT conn_id, used_at_ms FROM recent_connections \
                 ORDER BY used_at_ms DESC, conn_id LIMIT ?1",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }

    /// Forgets `conn_id` (a deleted connection).
    pub fn forget_recent(&self, conn_id: &str) -> Result<(), StoreError> {
        self.conn
            .lock()
            .execute(
                "DELETE FROM recent_connections WHERE conn_id = ?1",
                [conn_id],
            )
            .map(drop)
            .map_err(db_err)
    }
}
