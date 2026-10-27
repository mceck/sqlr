//! `SidecarDriver`: a [`Driver`] whose database lives in a child process. The engine cannot
//! tell it from an in-process driver (spec §6.2).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use parking_lot::Mutex;
use sqlr_core::{
    CancelHandle, ColumnMeta, Connection, ConnectionConfig, Dialect, Driver, DriverError,
    DriverManifest, ErrorKind, Execution, Node, NodePath, RelationalConn, ResultChunk, RowStream,
    SchemaCatalog, Secrets, ServerInfo, TableInfo, TableRef, Value, decode_chunk,
};

use crate::dialect::SidecarDialect;
use crate::messages::{Request, Response};
use crate::process::{ConnectGuard, Event, Process, SidecarSpec};

fn protocol_error(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Protocol, message)
}

fn unexpected(response: &Response) -> DriverError {
    protocol_error(format!("unexpected answer from the driver: {response:?}"))
}

/// A driver backed by a sidecar process, spawned on the first connection.
pub struct SidecarDriver {
    spec: SidecarSpec,
    manifest: DriverManifest,
    process: Mutex<Option<Arc<Process>>>,
    spawn: tokio::sync::Mutex<()>,
    next_conn: AtomicU64,
    /// How many times the process was restarted or respawned after an exit.
    restarts: AtomicU64,
    /// The dialect the first connection announced, for the app's editors and generated SQL.
    dialect: Mutex<Option<Arc<SidecarDialect>>>,
}

impl SidecarDriver {
    /// Starts the sidecar once to read its manifest, then stops it: the process comes back
    /// on the first connection.
    pub async fn probe(spec: SidecarSpec) -> Result<(Self, DriverManifest), DriverError> {
        let (process, manifest) = Process::start(&spec).await?;
        process.shutdown().await;
        Ok((Self::with_manifest(spec, manifest.clone()), manifest))
    }

    /// A driver whose manifest is already known (read from an install's `driver.toml`);
    /// nothing is spawned until a connection needs it.
    pub fn with_manifest(spec: SidecarSpec, manifest: DriverManifest) -> Self {
        Self {
            spec,
            manifest,
            process: Mutex::new(None),
            spawn: tokio::sync::Mutex::new(()),
            next_conn: AtomicU64::new(1),
            restarts: AtomicU64::new(0),
            dialect: Mutex::new(None),
        }
    }

    /// Kills the process. Its connections fail with a `Protocol` error; the next
    /// `connect` starts a fresh process.
    pub fn restart(&self) {
        if let Some(process) = self.process.lock().take() {
            self.restarts.fetch_add(1, Ordering::AcqRel);
            tracing::info!(
                target: "sqlr_sidecar::process",
                driver = %process.id(),
                "driver restarted"
            );
            process.kill(protocol_error(format!(
                "driver {} was restarted",
                self.spec.id
            )));
        }
    }

    /// How many times this driver's process was restarted or respawned.
    pub fn restart_count(&self) -> u64 {
        self.restarts.load(Ordering::Acquire)
    }

    /// The version of the installed driver.
    pub fn version(&self) -> &str {
        &self.spec.version
    }

    /// Whether the process is alive right now.
    pub fn is_running(&self) -> bool {
        self.process.lock().as_ref().is_some_and(|p| p.alive())
    }

    /// How the sidecar is started.
    pub fn spec(&self) -> &SidecarSpec {
        &self.spec
    }

    async fn process(&self) -> Result<Arc<Process>, DriverError> {
        let _spawning = self.spawn.lock().await;
        if let Some(process) = self.process.lock().as_ref().filter(|p| p.alive()) {
            return Ok(process.clone());
        }
        let had_process = self.process.lock().is_some();
        let (process, _) = Process::start(&self.spec).await?;
        if had_process {
            self.restarts.fetch_add(1, Ordering::AcqRel);
        }
        *self.process.lock() = Some(process.clone());
        Ok(process)
    }
}

#[async_trait]
impl Driver for SidecarDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    async fn connect(
        &self,
        cfg: &ConnectionConfig,
        secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        let process = self.process().await?;
        let conn = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let mut pending = ConnectGuard::new(process.clone(), Some(conn));
        let answer = process
            .call(&Request::Connect {
                conn,
                config: cfg.clone(),
                secrets: secrets.clone(),
            })
            .await?;
        if let Response::Connected {
            dialect: Some(spec),
            ..
        } = &answer
        {
            *self.dialect.lock() = Some(Arc::new(SidecarDialect::new(spec.clone())));
        }
        let connection = SidecarConnection::open(process, conn, answer)?;
        pending.complete();
        Ok(connection)
    }

    /// Known once a connection announced it; a sidecar's dialect is data, not code.
    fn dialect(&self) -> Option<Arc<dyn Dialect>> {
        self.dialect.lock().clone().map(|d| d as Arc<dyn Dialect>)
    }
}

/// The in-flight statements of one connection, shared with its cancel handle.
#[derive(Default)]
struct Inflight(AtomicUsize);

struct InflightGuard(Arc<Inflight>);

impl InflightGuard {
    fn enter(inflight: &Arc<Inflight>) -> Self {
        inflight.0.fetch_add(1, Ordering::AcqRel);
        Self(inflight.clone())
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One connection of a Sidecar driver: its calls go over the sidecar protocol to the driver process.
pub struct SidecarConnection {
    process: Arc<Process>,
    conn: u64,
    dialect: Option<SidecarDialect>,
    database: Option<String>,
    schema_path: NodePath,
    inflight: Arc<Inflight>,
    closed: AtomicBool,
}

impl SidecarConnection {
    fn open(
        process: Arc<Process>,
        conn: u64,
        answer: Response,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        let Response::Connected {
            dialect,
            database_name,
            schema_path,
            ..
        } = answer
        else {
            return Err(unexpected(&answer));
        };
        process.conn_opened();
        Ok(Arc::new(Self {
            process,
            conn,
            dialect: dialect.map(SidecarDialect::new),
            database: database_name,
            schema_path,
            inflight: Arc::default(),
            closed: AtomicBool::new(false),
        }))
    }

    async fn call(&self, request: Request) -> Result<Response, DriverError> {
        let _inflight = InflightGuard::enter(&self.inflight);
        self.process.call(&request).await
    }
}

impl Drop for SidecarConnection {
    fn drop(&mut self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let process = self.process.clone();
        let conn = self.conn;
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = process.call(&Request::Disconnect { conn }).await;
                    process.conn_closed();
                });
            }
            Err(_) => process.conn_closed(),
        }
    }
}

#[async_trait]
impl Connection for SidecarConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        match self.call(Request::Ping { conn: self.conn }).await? {
            Response::Pong(info) => Ok(info),
            other => Err(unexpected(&other)),
        }
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        match self
            .call(Request::Browse {
                conn: self.conn,
                path: path.clone(),
            })
            .await?
        {
            Response::Nodes(nodes) => Ok(nodes),
            other => Err(unexpected(&other)),
        }
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(SidecarCancel {
            process: self.process.clone(),
            conn: self.conn,
            inflight: self.inflight.clone(),
        })
    }

    fn relational(&self) -> Option<&dyn RelationalConn> {
        self.dialect.as_ref().map(|_| self as &dyn RelationalConn)
    }

    fn database_name(&self) -> Option<&str> {
        self.database.as_deref()
    }

    fn schema_path(&self) -> NodePath {
        self.schema_path.clone()
    }

    async fn connect_database(&self, database: &str) -> Result<Arc<dyn Connection>, DriverError> {
        // Ids of derived connections come from the high half so they never meet the
        // driver's own counter.
        static NEXT: AtomicU64 = AtomicU64::new(1 << 40);
        let new_conn = NEXT.fetch_add(1, Ordering::Relaxed);
        let answer = self
            .call(Request::ConnectDatabase {
                conn: self.conn,
                new_conn,
                database: database.to_string(),
            })
            .await?;
        SidecarConnection::open(self.process.clone(), new_conn, answer)
    }
}

struct SidecarCancel {
    process: Arc<Process>,
    conn: u64,
    inflight: Arc<Inflight>,
}

#[async_trait]
impl CancelHandle for SidecarCancel {
    /// Asks the driver to cancel; a statement still running after the grace period means the
    /// driver ignores cancels, and the process is killed.
    async fn cancel(&self) -> Result<(), DriverError> {
        let grace = self.process.cancel_grace;
        let deadline = Instant::now() + grace;
        // A driver that does not even answer the cancel is as bad as one that ignores it.
        match tokio::time::timeout(
            grace,
            self.process.call(&Request::Cancel { conn: self.conn }),
        )
        .await
        {
            Ok(answer) => {
                answer?;
            }
            Err(_) => {
                self.process.kill(DriverError::new(
                    ErrorKind::Cancelled,
                    format!(
                        "driver {} did not answer the cancel and was killed",
                        self.process.id()
                    ),
                ));
                return Ok(());
            }
        }
        while self.inflight.0.load(Ordering::Acquire) > 0 {
            if Instant::now() >= deadline {
                self.process.kill(DriverError::new(
                    ErrorKind::Cancelled,
                    format!(
                        "driver {} did not stop the statement after cancel and was killed",
                        self.process.id()
                    ),
                ));
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok(())
    }
}

#[async_trait]
impl RelationalConn for SidecarConnection {
    async fn execute(&self, sql: &str) -> Result<Execution, DriverError> {
        self.run(sql, None).await
    }

    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<Execution, DriverError> {
        self.run(sql, Some(params.to_vec())).await
    }

    async fn describe(&self, table: &TableRef) -> Result<TableInfo, DriverError> {
        match self
            .call(Request::Describe {
                conn: self.conn,
                table: table.clone(),
            })
            .await?
        {
            Response::Table(info) => Ok(info),
            other => Err(unexpected(&other)),
        }
    }

    async fn take_notices(&self) -> Vec<String> {
        match self.call(Request::TakeNotices { conn: self.conn }).await {
            Ok(Response::Notices(notices)) => notices,
            _ => Vec::new(),
        }
    }

    fn dialect(&self) -> &dyn Dialect {
        self.dialect
            .as_ref()
            .expect("relational() is Some only with a dialect")
    }

    async fn catalog(&self, schema: &str) -> Result<SchemaCatalog, DriverError> {
        match self
            .call(Request::Catalog {
                conn: self.conn,
                schema: schema.to_string(),
            })
            .await?
        {
            Response::Catalog(catalog) => Ok(catalog),
            other => Err(unexpected(&other)),
        }
    }
}

impl SidecarConnection {
    async fn run(&self, sql: &str, params: Option<Vec<Value>>) -> Result<Execution, DriverError> {
        let answer = self
            .call(Request::Execute {
                conn: self.conn,
                sql: sql.to_string(),
                params,
            })
            .await?;
        match answer {
            Response::Affected(n) => Ok(Execution::Affected(n)),
            Response::Rows { stream, columns } => Ok(Execution::Rows(Box::new(SidecarStream {
                process: self.process.clone(),
                inflight: self.inflight.clone(),
                stream,
                columns: Arc::from(columns),
                ready: VecDeque::new(),
                exhausted: false,
                closed: false,
            }))),
            other => Err(unexpected(&other)),
        }
    }
}

struct SidecarStream {
    process: Arc<Process>,
    inflight: Arc<Inflight>,
    stream: u64,
    columns: Arc<[ColumnMeta]>,
    ready: VecDeque<ResultChunk>,
    exhausted: bool,
    closed: bool,
}

#[async_trait]
impl RowStream for SidecarStream {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }

    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        if let Some(chunk) = self.ready.pop_front() {
            return Ok(Some(chunk));
        }
        if self.exhausted || self.closed {
            return Ok(None);
        }
        let _inflight = InflightGuard::enter(&self.inflight);
        let mut pending = self.process.begin(&Request::Fetch {
            stream: self.stream,
            max: u32::try_from(max_rows).unwrap_or(u32::MAX),
        })?;
        loop {
            match pending.next().await? {
                Event::Chunk(bytes) => {
                    let chunk = decode_chunk(&bytes).map_err(|e| {
                        protocol_error(format!(
                            "driver {} sent a malformed chunk: {e}",
                            self.process.id()
                        ))
                    })?;
                    if *chunk.columns() != self.columns {
                        let e = protocol_error(format!(
                            "driver {} sent a chunk that does not match the result's columns",
                            self.process.id()
                        ));
                        self.process.kill(e.clone());
                        return Err(e);
                    }
                    self.ready.push_back(chunk);
                }
                Event::Control(response) => match *response {
                    Response::FetchDone { exhausted } => {
                        self.exhausted = exhausted;
                        return Ok(self.ready.pop_front());
                    }
                    Response::Error(e) => return Err(e),
                    other => return Err(unexpected(&other)),
                },
                Event::Closed(e) => return Err(e),
            }
        }
    }

    async fn close(&mut self) -> Result<(), DriverError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.ready.clear();
        if self.exhausted || !self.process.alive() {
            return Ok(());
        }
        self.process
            .call(&Request::CloseStream {
                stream: self.stream,
            })
            .await
            .map(|_| ())
    }
}

impl Drop for SidecarStream {
    fn drop(&mut self) {
        if self.closed || self.exhausted || !self.process.alive() {
            return;
        }
        let process = self.process.clone();
        let stream = self.stream;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = process.call(&Request::CloseStream { stream }).await;
            });
        }
    }
}
