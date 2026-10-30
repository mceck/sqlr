//! A real SQLite stream paused while the sidecar obtains its columns.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use parking_lot::{Condvar, Mutex};
use sqlr_core::{
    CancelHandle, ColumnMeta, Connection, ConnectionConfig, Dialect, Driver, DriverError,
    DriverManifest, Execution, Node, NodePath, RelationalConn, ResultChunk, RowStream, Secrets,
    ServerInfo,
};
use sqlr_driver_sqlite::SqliteDriver;

#[derive(Default)]
pub struct Gate {
    pub entered: tokio::sync::Notify,
    released: Mutex<bool>,
    wake: Condvar,
    pub closed: AtomicUsize,
}

impl Gate {
    pub fn release(&self) {
        *self.released.lock() = true;
        self.wake.notify_all();
    }

    fn pause(&self) {
        self.entered.notify_one();
        let mut released = self.released.lock();
        while !*released {
            self.wake.wait(&mut released);
        }
    }
}

pub struct PausingDriver(pub Arc<Gate>, pub Arc<SqliteDriver>);

#[async_trait]
impl Driver for PausingDriver {
    fn manifest(&self) -> &DriverManifest {
        self.1.manifest()
    }

    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        Ok(Arc::new(PausingConnection(
            self.0.clone(),
            self.1.connect(cfg, secrets).await?,
        )))
    }
}

struct PausingConnection(Arc<Gate>, Arc<dyn Connection>);

#[async_trait]
impl Connection for PausingConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        self.1.ping().await
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        self.1.browse(path).await
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.1.cancel_handle()
    }

    fn relational(&self) -> Option<&dyn RelationalConn> {
        Some(self)
    }
}

#[async_trait]
impl RelationalConn for PausingConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        match self.1.relational().unwrap().execute(sql).await? {
            Execution::Rows(stream) => Ok(Execution::Rows(Box::new(PausingStream(
                self.0.clone(),
                stream,
            )))),
            other => Ok(other),
        }
    }

    fn dialect(&self) -> &dyn Dialect {
        self.1.relational().unwrap().dialect()
    }
}

struct PausingStream(Arc<Gate>, Box<dyn RowStream>);

#[async_trait]
impl RowStream for PausingStream {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.0.pause();
        self.1.columns()
    }

    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        self.1.next_chunk(max_rows).await
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        self.0.closed.fetch_add(1, Ordering::SeqCst);
        self.1.close().await
    }
}
