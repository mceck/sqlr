use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::params;
use sqlr_core::SchemaCatalog;

use crate::StoreError;
use crate::db::{Db, db_err};

/// The persisted catalog of one connection.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredCatalog {
    pub conn_id: String,
    /// `None` when the schema list was never saved.
    pub schemas: Option<Vec<String>>,
    pub schema: Vec<SchemaCatalog>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl Db {
    pub fn save_catalog_schemas(
        &self,
        conn_id: &str,
        schemas: &[String],
    ) -> Result<(), StoreError> {
        let body = serde_json::to_string(schemas).map_err(db_err)?;
        self.conn
            .lock()
            .execute(
                "INSERT OR REPLACE INTO catalog_schemas (conn_id, body, saved_at) VALUES (?1, ?2, ?3)",
                params![conn_id, body, now_ms()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn save_schema_catalog(
        &self,
        conn_id: &str,
        cat: &SchemaCatalog,
    ) -> Result<(), StoreError> {
        let body = serde_json::to_string(cat).map_err(db_err)?;
        self.conn
            .lock()
            .execute(
                "INSERT OR REPLACE INTO catalog_schema (conn_id, schema, body, saved_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![conn_id, cat.schema, body, now_ms()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Every cached catalog, sorted by connection id. Rows whose JSON no longer
    /// parses are skipped: the cache is refreshed from the server anyway.
    pub fn load_catalogs(&self) -> Result<Vec<StoredCatalog>, StoreError> {
        let conn = self.conn.lock();
        let mut out: BTreeMap<String, StoredCatalog> = BTreeMap::new();
        let mut schemas_rows: Vec<(String, String)> = Vec::new();
        {
            let mut stmt = conn
                .prepare_cached("SELECT conn_id, body FROM catalog_schemas")
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(db_err)?;
            for row in rows {
                schemas_rows.push(row.map_err(db_err)?);
            }
        }
        let mut schema_rows: Vec<(String, String)> = Vec::new();
        {
            let mut stmt = conn
                .prepare_cached("SELECT conn_id, body FROM catalog_schema ORDER BY conn_id, schema")
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(db_err)?;
            for row in rows {
                schema_rows.push(row.map_err(db_err)?);
            }
        }
        let slot = |out: &mut BTreeMap<String, StoredCatalog>, id: String| {
            out.entry(id.clone()).or_insert(StoredCatalog {
                conn_id: id,
                schemas: None,
                schema: Vec::new(),
            });
        };
        for (id, body) in schemas_rows {
            if let Ok(list) = serde_json::from_str::<Vec<String>>(&body) {
                slot(&mut out, id.clone());
                out.get_mut(&id).expect("slot").schemas = Some(list);
            }
        }
        for (id, body) in schema_rows {
            if let Ok(cat) = serde_json::from_str::<SchemaCatalog>(&body) {
                slot(&mut out, id.clone());
                out.get_mut(&id).expect("slot").schema.push(cat);
            }
        }
        Ok(out.into_values().collect())
    }

    pub fn delete_catalog(&self, conn_id: &str) -> Result<(), StoreError> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute("DELETE FROM catalog_schemas WHERE conn_id = ?1", [conn_id])
            .map_err(db_err)?;
        tx.execute("DELETE FROM catalog_schema WHERE conn_id = ?1", [conn_id])
            .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }
}
