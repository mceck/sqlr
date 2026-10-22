//! ER diagram layouts, one per (connection, database, schema, table set), in `sqlr.db`.

use rusqlite::{OptionalExtension, params};

use crate::StoreError;
use crate::db::{Db, db_err};

impl Db {
    /// The saved layout (JSON the caller wrote) under `key`.
    pub fn load_er_layout(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.conn
            .lock()
            .query_row(
                "SELECT layout FROM er_layouts WHERE layout_key = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)
    }

    /// Saves (or replaces) the layout under `key`.
    pub fn save_er_layout(&self, key: &str, json: &str) -> Result<(), StoreError> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO er_layouts (layout_key, layout) VALUES (?1, ?2) \
                 ON CONFLICT(layout_key) DO UPDATE SET layout = excluded.layout",
                params![key, json],
            )
            .map_err(db_err)?;
        Ok(())
    }
}
