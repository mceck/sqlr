use rusqlite::params;
use sqlr_core::library::Snippet;

use crate::StoreError;
use crate::db::{Db, db_err};

impl Db {
    /// Sorted by `(folder, name)`.
    pub fn list_snippets(&self) -> Result<Vec<Snippet>, StoreError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, name, folder, conn_id, sql, updated_at FROM snippets \
                 ORDER BY folder, name, id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Snippet {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    folder: r.get(2)?,
                    conn_id: r.get(3)?,
                    sql: r.get(4)?,
                    updated_at_ms: r.get(5)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }

    /// Inserts, or replaces the snippet with the same id.
    pub fn save_snippet(&self, s: &Snippet) -> Result<(), StoreError> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO snippets (id, name, folder, conn_id, sql, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, folder = excluded.folder, \
                 conn_id = excluded.conn_id, sql = excluded.sql, updated_at = excluded.updated_at",
                params![s.id, s.name, s.folder, s.conn_id, s.sql, s.updated_at_ms],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn delete_snippet(&self, id: &str) -> Result<(), StoreError> {
        self.conn
            .lock()
            .execute("DELETE FROM snippets WHERE id = ?1", [id])
            .map_err(db_err)?;
        Ok(())
    }
}
