//! Recently opened relations (the object Palette's first rows): which table or view, on
//! which connection and database, and when. Only the newest [`RECENT_OBJECTS_KEEP`] stay.

use rusqlite::params;
use sqlr_core::{RecentObject, RelationKind};

use crate::StoreError;
use crate::db::{Db, db_err};

/// Recently opened relations kept.
pub const RECENT_OBJECTS_KEEP: usize = 20;

fn kind_text(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Table => "table",
        RelationKind::View => "view",
        RelationKind::MaterializedView => "materialized_view",
    }
}

fn kind_from(text: &str) -> Option<RelationKind> {
    match text {
        "table" => Some(RelationKind::Table),
        "view" => Some(RelationKind::View),
        "materialized_view" => Some(RelationKind::MaterializedView),
        _ => None,
    }
}

impl Db {
    /// Records that `object` was opened at its `used_at_ms`, and forgets all but the newest
    /// [`RECENT_OBJECTS_KEEP`].
    pub fn touch_recent_object(&self, object: &RecentObject) -> Result<(), StoreError> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute(
            "INSERT INTO recent_objects (conn_id, database, schema, name, kind, used_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(conn_id, database, schema, name) \
             DO UPDATE SET kind = excluded.kind, used_at_ms = excluded.used_at_ms",
            params![
                object.conn_id,
                object.database.as_deref().unwrap_or(""),
                object.schema,
                object.name,
                kind_text(object.kind),
                object.used_at_ms,
            ],
        )
        .map_err(db_err)?;
        tx.execute(
            "DELETE FROM recent_objects WHERE rowid NOT IN \
             (SELECT rowid FROM recent_objects ORDER BY used_at_ms DESC, rowid DESC LIMIT ?1)",
            [i64::try_from(RECENT_OBJECTS_KEEP).unwrap_or(i64::MAX)],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// The recently opened relations, most recent first. Rows that no longer parse are
    /// skipped.
    pub fn recent_objects(&self) -> Result<Vec<RecentObject>, StoreError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT conn_id, database, schema, name, kind, used_at_ms FROM recent_objects \
                 ORDER BY used_at_ms DESC, rowid DESC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                let database: String = r.get(1)?;
                let kind: String = r.get(4)?;
                let Some(kind) = kind_from(&kind) else {
                    return Ok(None);
                };
                Ok(Some(RecentObject {
                    conn_id: r.get(0)?,
                    database: (!database.is_empty()).then_some(database),
                    schema: r.get(2)?,
                    name: r.get(3)?,
                    kind,
                    used_at_ms: r.get(5)?,
                }))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            if let Some(object) = row.map_err(db_err)? {
                out.push(object);
            }
        }
        Ok(out)
    }

    /// Forgets the recently opened relations of `conn_id` (a deleted connection).
    pub fn forget_recent_objects(&self, conn_id: &str) -> Result<(), StoreError> {
        self.conn
            .lock()
            .execute("DELETE FROM recent_objects WHERE conn_id = ?1", [conn_id])
            .map(drop)
            .map_err(db_err)
    }
}
