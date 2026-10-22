//! One running sidecar process: its pipes, the requests waiting for answers and its
//! lifecycle (handshake, idle stop, kill).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use sqlr_core::{DriverError, DriverManifest, ErrorKind};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use crate::frame::{CHUNK, CONTROL, DOC_CHUNK, LOG, encode_frame, read_frame};
use crate::messages::{PROTOCOL, Request, Response, decode, encode};

/// How to start a sidecar and how long to wait for it.
#[derive(Clone, Debug)]
pub struct SidecarSpec {
    pub id: String,
    pub version: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The process stops this long after its last connection closes.
    pub idle_timeout: Duration,
    /// How long the handshake may take before the process is killed.
    pub handshake_timeout: Duration,
    /// How long a statement may outlive a `Cancel` before the process is killed.
    pub cancel_grace: Duration,
}

impl SidecarSpec {
    pub fn new(
        id: impl Into<String>,
        version: impl Into<String>,
        program: impl Into<PathBuf>,
    ) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            idle_timeout: Duration::from_secs(300),
            handshake_timeout: Duration::from_secs(10),
            cancel_grace: Duration::from_secs(5),
        }
    }
}

pub(crate) enum Event {
    Control(Box<Response>),
    Chunk(Vec<u8>),
    Closed(DriverError),
}

pub(crate) struct Process {
    id: String,
    next_request: AtomicU64,
    pending: Mutex<HashMap<u64, mpsc::UnboundedSender<Event>>>,
    out: mpsc::UnboundedSender<Vec<u8>>,
    alive: AtomicBool,
    reason: Mutex<Option<DriverError>>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
    conns: AtomicUsize,
    connecting: AtomicUsize,
    idle_epoch: AtomicU64,
    idle_timeout: Duration,
    pub(crate) cancel_grace: Duration,
}

fn protocol_error(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Protocol, message)
}

/// A request waiting for its answers; dropping it forgets the request.
pub(crate) struct Pending {
    process: Arc<Process>,
    id: u64,
    rx: mpsc::UnboundedReceiver<Event>,
}

impl Pending {
    pub(crate) async fn next(&mut self) -> Result<Event, DriverError> {
        match self.rx.recv().await {
            Some(Event::Closed(e)) => Err(e),
            Some(event) => Ok(event),
            None => Err(self.process.dead_error()),
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.process.pending.lock().remove(&self.id);
    }
}

/// Owns a pending handshake or connection until its future completes.
pub(crate) struct ConnectGuard {
    process: Arc<Process>,
    conn: Option<u64>,
    complete: bool,
}

impl ConnectGuard {
    pub(crate) fn new(process: Arc<Process>, conn: Option<u64>) -> Self {
        process.connecting.fetch_add(1, Ordering::AcqRel);
        Self {
            process,
            conn,
            complete: false,
        }
    }
    pub(crate) fn complete(&mut self) {
        self.complete = true;
    }
}

impl Drop for ConnectGuard {
    fn drop(&mut self) {
        let last = self.process.connecting.fetch_sub(1, Ordering::AcqRel) == 1;
        if self.complete {
            return;
        }
        if last && self.process.conns.load(Ordering::Acquire) == 0 {
            self.process.kill(DriverError::new(
                ErrorKind::Cancelled,
                "Connection cancelled",
            ));
        } else if let Some(conn) = self.conn {
            let _ = self.process.begin(&Request::Disconnect { conn });
        }
    }
}

fn major(version: &str) -> Option<u64> {
    version.split('.').next()?.parse().ok()
}

impl Process {
    /// Spawns the sidecar and completes the handshake.
    pub(crate) async fn start(
        spec: &SidecarSpec,
    ) -> Result<(Arc<Process>, DriverManifest), DriverError> {
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                DriverError::new(
                    ErrorKind::Connection,
                    format!(
                        "cannot start driver {} ({}): {e}",
                        spec.id,
                        spec.program.display()
                    ),
                )
            })?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (out, mut queue) = mpsc::unbounded_channel::<Vec<u8>>();
        let (kill_tx, kill_rx) = oneshot::channel();
        let process = Arc::new(Process {
            id: spec.id.clone(),
            next_request: AtomicU64::new(1),
            pending: Mutex::default(),
            out,
            alive: AtomicBool::new(true),
            reason: Mutex::new(None),
            kill: Mutex::new(Some(kill_tx)),
            conns: AtomicUsize::new(0),
            connecting: AtomicUsize::new(0),
            idle_epoch: AtomicU64::new(0),
            idle_timeout: spec.idle_timeout,
            cancel_grace: spec.cancel_grace,
        });

        // Writer: requests to the child's stdin.
        tokio::spawn(async move {
            while let Some(frame) = queue.recv().await {
                if stdin.write_all(&frame).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
        // Its stderr is the driver's log.
        let id = spec.id.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!(target: "sqlr_sidecar::driver", "[{id}] {line}");
            }
        });
        // Reader: answers from the child's stdout.
        let reader = process.clone();
        tokio::spawn(async move {
            let mut stdout = stdout;
            loop {
                match read_frame(&mut stdout).await {
                    Ok(Some(frame)) => match frame.ty {
                        CONTROL => match decode::<Response>(&frame.payload) {
                            Ok(response) => {
                                reader.deliver(frame.id, Event::Control(Box::new(response)));
                            }
                            Err(e) => {
                                reader.kill(protocol_error(format!(
                                    "driver {} sent a malformed message: {e}",
                                    reader.id
                                )));
                                break;
                            }
                        },
                        CHUNK => reader.deliver(frame.id, Event::Chunk(frame.payload)),
                        LOG => tracing::info!(
                            target: "sqlr_sidecar::driver",
                            "[{}] {}",
                            reader.id,
                            String::from_utf8_lossy(&frame.payload)
                        ),
                        DOC_CHUNK => tracing::warn!("[{}] document chunk ignored", reader.id),
                        _ => {}
                    },
                    Ok(None) => {
                        reader.fail_all(protocol_error(format!(
                            "driver {} exited unexpectedly",
                            reader.id
                        )));
                        break;
                    }
                    Err(e) => {
                        reader.kill(protocol_error(format!(
                            "driver {} broke the protocol: {e}",
                            reader.id
                        )));
                        break;
                    }
                }
            }
        });
        // Owner of the child: waits for its exit, or kills it on request.
        let waiter = process.clone();
        tokio::spawn(async move {
            tokio::select! {
                status = child.wait() => {
                    let how = status.map_or_else(|e| e.to_string(), |s| s.to_string());
                    waiter.fail_all(protocol_error(format!(
                        "driver {} exited unexpectedly ({how})", waiter.id
                    )));
                }
                _ = kill_rx => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
        });

        let mut startup = ConnectGuard::new(process.clone(), None);
        let hello = Request::Hello {
            protocol: PROTOCOL.to_string(),
            sqlr_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let welcome = tokio::time::timeout(spec.handshake_timeout, process.call(&hello)).await;
        let welcome = match welcome {
            Ok(result) => result,
            Err(_) => {
                let e = DriverError::new(
                    ErrorKind::Timeout,
                    format!(
                        "driver {} did not answer the handshake within {:?}",
                        spec.id, spec.handshake_timeout
                    ),
                );
                process.kill(e.clone());
                return Err(e);
            }
        };
        let (protocol, manifest) = match welcome {
            Ok(Response::Welcome { protocol, manifest }) => (protocol, manifest),
            Ok(other) => {
                let e = protocol_error(format!("driver {} answered Hello with {other:?}", spec.id));
                process.kill(e.clone());
                return Err(e);
            }
            Err(e) => {
                process.kill(e.clone());
                return Err(e);
            }
        };
        if major(&protocol) != major(PROTOCOL) {
            let e = protocol_error(format!(
                "Driver {} speaks protocol {protocol}, sqlr speaks {PROTOCOL}",
                spec.id
            ));
            process.kill(e.clone());
            return Err(e);
        }
        startup.complete();
        Ok((process, manifest))
    }

    pub(crate) fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    fn dead_error(&self) -> DriverError {
        self.reason
            .lock()
            .clone()
            .unwrap_or_else(|| protocol_error(format!("driver {} is not running", self.id)))
    }

    fn deliver(&self, id: u64, event: Event) {
        if let Some(tx) = self.pending.lock().get(&id) {
            let _ = tx.send(event);
        }
    }

    /// Marks the process dead and fails every waiting request with `error`.
    fn fail_all(&self, error: DriverError) {
        let first = {
            let mut reason = self.reason.lock();
            if reason.is_none() {
                *reason = Some(error.clone());
                true
            } else {
                false
            }
        };
        self.alive.store(false, Ordering::Release);
        let reason = if first { error } else { self.dead_error() };
        let waiting: Vec<_> = self.pending.lock().drain().collect();
        for (_, tx) in waiting {
            let _ = tx.send(Event::Closed(reason.clone()));
        }
    }

    /// Kills the process; its requests fail with `error`.
    pub(crate) fn kill(&self, error: DriverError) {
        self.fail_all(error);
        if let Some(tx) = self.kill.lock().take() {
            let _ = tx.send(());
        }
    }

    /// Sends `request` and registers for its answers.
    pub(crate) fn begin(self: &Arc<Self>, request: &Request) -> Result<Pending, DriverError> {
        if !self.alive() {
            return Err(self.dead_error());
        }
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        self.pending.lock().insert(id, tx);
        // Dropping a `Pending` forgets the request, including when sending fails.
        let pending = Pending {
            process: self.clone(),
            id,
            rx,
        };
        tracing::trace!(target: "sqlr_sidecar::protocol", driver = %self.id, id, ?request, "request");
        let mut frame = Vec::new();
        encode_frame(CONTROL, id, &encode(request), &mut frame)
            .map_err(|e| protocol_error(e.to_string()))?;
        self.out.send(frame).map_err(|_| self.dead_error())?;
        // The process may have died between the check and the registration.
        if !self.alive() {
            return Err(self.dead_error());
        }
        Ok(pending)
    }

    /// A request with one control answer; `Error` answers become `Err`.
    pub(crate) async fn call(self: &Arc<Self>, request: &Request) -> Result<Response, DriverError> {
        let mut pending = self.begin(request)?;
        match pending.next().await? {
            Event::Control(response) => match *response {
                Response::Error(e) => Err(e),
                response => Ok(response),
            },
            Event::Chunk(_) => Err(protocol_error("a chunk answered a request that takes none")),
            Event::Closed(e) => Err(e),
        }
    }

    pub(crate) fn conn_opened(&self) {
        self.conns.fetch_add(1, Ordering::AcqRel);
        self.idle_epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// A connection closed; the last one starts the idle timer.
    pub(crate) fn conn_closed(self: &Arc<Self>) {
        if self.conns.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        let epoch = self.idle_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        let me = self.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            tokio::time::sleep(me.idle_timeout).await;
            if me.conns.load(Ordering::Acquire) == 0
                && me.idle_epoch.load(Ordering::Acquire) == epoch
            {
                me.shutdown().await;
            }
        });
    }

    /// Asks the driver to exit, then makes sure it did.
    pub(crate) async fn shutdown(self: &Arc<Self>) {
        if self.alive() {
            let _ = tokio::time::timeout(Duration::from_millis(500), self.call(&Request::Shutdown))
                .await;
        }
        self.kill(protocol_error(format!("driver {} was stopped", self.id)));
    }
}
