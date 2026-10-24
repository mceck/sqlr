//! Schema compare jobs (spec §8.6): two schemas are read in bulk and compared; the
//! migration script is written for the target. Nothing is ever written to either database.

use std::collections::HashSet;
use std::sync::Arc;

use sqlr_core::{
    DriverError, ErrorKind, ObjectDiff, ObjectRef, SchemaSnapshot, compare, migration_script,
};

use crate::engine::{Command, Engine, Shared, not_connected};
use crate::job::{JobControl, JobId, JobKind, JobOutcome};
use crate::session::SessionKey;

/// One side of a compare: a connection, a database on it (`None`: its own) and a schema.
pub type SchemaSide = (String, Option<String>, String);

fn unsupported(message: &str) -> DriverError {
    DriverError::new(ErrorKind::Unsupported, message)
}

impl Engine {
    /// Compares `source` with `target` as a job ([`JobOutcome::Compared`]): the differences
    /// that make the target look like the source. Both connections must be connected and of
    /// the same kind of database.
    pub fn compare_schemas(&self, source: SchemaSide, target: SchemaSide) -> JobId {
        let job = self.shared.jobs.create(JobKind::Compare);
        let id = job.id;
        let task = self.shared.clone().run_compare(job, source, target);
        self.shared.send(Command::job(id, None, task));
        self.shared.repaint();
        id
    }

    /// The tables (all, or only those of `tables`) and foreign keys of a schema as a job
    /// ([`JobOutcome::ErModel`]).
    pub fn er_model(
        &self,
        conn: &str,
        database: Option<String>,
        schema: String,
        tables: Option<Vec<String>>,
    ) -> JobId {
        let job = self.shared.jobs.create(JobKind::ErModel);
        let id = job.id;
        let side = (conn.to_string(), database, schema);
        let shared = self.shared.clone();
        let task = async move {
            let outcome = match shared.snapshot(&side).await {
                Ok(snapshot) => JobOutcome::ErModel(sqlr_core::ErModel::from_snapshot(
                    &snapshot,
                    tables.as_deref(),
                )),
                Err(e) => JobOutcome::Failed(e),
            };
            shared.jobs.finish(job.id, outcome);
            shared.repaint();
        };
        self.shared
            .send(Command::job(id, Some(conn.to_string()), task));
        self.shared.repaint();
        id
    }

    /// The saved ER layout under `key` as a job ([`JobOutcome::ErLayout`]).
    pub fn load_er_layout(&self, key: &str) -> JobId {
        let job = self.shared.jobs.create(JobKind::ErModel);
        let id = job.id;
        let key = key.to_string();
        let shared = self.shared.clone();
        self.shared.db.push(move |db| {
            let outcome = match db.load_er_layout(&key) {
                Ok(layout) => JobOutcome::ErLayout(layout),
                Err(e) => JobOutcome::Failed(DriverError::new(ErrorKind::Other, e.to_string())),
            };
            shared.jobs.finish(id, outcome);
            shared.repaint();
        });
        id
    }

    /// Saves an ER layout (JSON) under `key`, behind.
    pub fn save_er_layout(&self, key: &str, json: &str) {
        let (key, json) = (key.to_string(), json.to_string());
        self.shared.db.push(move |db| {
            if let Err(e) = db.save_er_layout(&key, &json) {
                tracing::warn!("could not save an ER layout: {e}");
            }
        });
    }

    /// The migration script for `diffs` (without the `excluded` objects) in `conn`'s dialect:
    /// `conn` is the target of the compare. It only writes text.
    pub fn migration_script(
        &self,
        conn: &str,
        diffs: &[ObjectDiff],
        excluded: &HashSet<ObjectRef>,
        header: &str,
    ) -> Result<String, DriverError> {
        let connection = self.meta_conn(conn)?;
        let generator = connection
            .relational()
            .and_then(|rel| rel.dialect().ddl_gen())
            .ok_or_else(|| unsupported("this connection cannot write DDL"))?;
        Ok(migration_script(diffs, excluded, generator, header))
    }
}

impl Shared {
    async fn snapshot(&self, side: &SchemaSide) -> Result<SchemaSnapshot, DriverError> {
        let (conn, database, schema) = side;
        let io = self.live_io(conn).ok_or_else(not_connected)?;
        let connection = self
            .connected(&SessionKey::meta(conn), &io)
            .await
            .ok_or_else(not_connected)?;
        let introspect = connection
            .relational()
            .and_then(|rel| rel.introspect())
            .ok_or_else(|| unsupported("this connection cannot be compared"))?;
        introspect.load_schema(database.as_deref(), schema).await
    }

    async fn run_compare(self: Arc<Self>, job: JobControl, source: SchemaSide, target: SchemaSide) {
        let outcome = self.compare_sides(&source, &target).await;
        let outcome = match outcome {
            Ok(diffs) => JobOutcome::Compared(diffs),
            Err(e) => JobOutcome::Failed(e),
        };
        self.jobs.finish(job.id, outcome);
        self.repaint();
    }

    async fn compare_sides(
        &self,
        source: &SchemaSide,
        target: &SchemaSide,
    ) -> Result<Vec<ObjectDiff>, DriverError> {
        let (a, b) = (
            self.driver_of_conn(&source.0),
            self.driver_of_conn(&target.0),
        );
        match (a, b) {
            (Some(a), Some(b)) if a == b => {}
            (Some(_), Some(_)) => {
                return Err(unsupported(
                    "Schema compare needs two connections of the same kind of database",
                ));
            }
            _ => return Err(not_connected()),
        }
        let from = self.snapshot(source).await?;
        let to = self.snapshot(target).await?;
        let io = self.live_io(&target.0).ok_or_else(not_connected)?;
        let connection = self
            .connected(&SessionKey::meta(&target.0), &io)
            .await
            .ok_or_else(not_connected)?;
        let generator = connection
            .relational()
            .and_then(|rel| rel.dialect().ddl_gen())
            .ok_or_else(|| unsupported("this connection cannot write DDL"))?;
        Ok(compare(&from, &to, generator))
    }

    fn driver_of_conn(&self, conn: &str) -> Option<String> {
        self.inner.lock().driver_of(conn)
    }
}
