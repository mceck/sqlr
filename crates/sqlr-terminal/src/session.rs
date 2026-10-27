//! The PTY session: a reader thread feeds the parser, a monitor thread reports the exit,
//! and the UI only writes input and snapshots the screen.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::screen::{Emulator, ScreenSnapshot};
use crate::shell::{CommandSpec, resolve_current};

/// What the panel shows about the child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalStatus {
    Starting,
    Running { pid: Option<u32> },
    Exited { code: u32 },
    Failed { message: String },
}

impl TerminalStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Starting | Self::Running { .. })
    }
}

/// Why a terminal backend could not start or wait for its child.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    /// The pseudo-terminal library refused to open, spawn or clone a handle.
    #[error("{0}")]
    Backend(Box<dyn std::error::Error + Send + Sync>),
    /// Waiting for the child failed.
    #[error("{0}")]
    Wait(#[from] std::io::Error),
}

/// The pieces a backend hands the session. The reader and writer are owned by the session's
/// threads; the closures keep the backend alive.
pub struct BackendParts {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub resize: Box<dyn Fn(u16, u16) + Send + Sync>,
    pub kill: Box<dyn Fn() + Send + Sync>,
    /// Blocks until the child exits (run on the monitor thread).
    pub wait: Box<dyn FnOnce() -> Result<u32, PtyError> + Send>,
    pub pid: Option<u32>,
}

pub trait BackendFactory: Send + Sync {
    fn start(&self, spec: &CommandSpec, rows: u16, cols: u16) -> Result<BackendParts, PtyError>;
}

/// The settings a session starts with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    pub shell: String,
    pub cwd: String,
    pub scrollback: usize,
    pub rows: u16,
    pub cols: u16,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            shell: String::new(),
            cwd: String::new(),
            scrollback: 2000,
            rows: 24,
            cols: 80,
        }
    }
}

pub struct TerminalSession {
    emulator: Arc<Emulator>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    resize_fn: Box<dyn Fn(u16, u16) + Send + Sync>,
    kill_fn: Arc<dyn Fn() + Send + Sync>,
    status: Arc<Mutex<TerminalStatus>>,
    reader: Option<JoinHandle<()>>,
    monitor: Option<JoinHandle<()>>,
    size: (u16, u16),
    /// Whether the app asked to kill the child, so a later exit is expected.
    killed: Arc<AtomicBool>,
}

impl TerminalSession {
    pub fn spawn(config: &SessionConfig, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let spec = resolve_current(&config.shell, &config.cwd);
        Self::spawn_with(&crate::pty::PortablePty, &spec, config, repaint)
    }

    pub fn spawn_with(
        factory: &dyn BackendFactory,
        spec: &CommandSpec,
        config: &SessionConfig,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let status = Arc::new(Mutex::new(TerminalStatus::Starting));
        let emulator = Arc::new(Emulator::new(config.rows, config.cols, config.scrollback));
        let parts = match factory.start(spec, config.rows, config.cols) {
            Ok(parts) => parts,
            Err(error) => {
                *status.lock() = TerminalStatus::Failed {
                    message: error.to_string(),
                };
                return Self {
                    emulator,
                    writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
                    resize_fn: Box::new(|_, _| {}),
                    kill_fn: Arc::new(|| {}),
                    status,
                    reader: None,
                    monitor: None,
                    size: (config.rows, config.cols),
                    killed: Arc::new(AtomicBool::new(false)),
                };
            }
        };
        let kill: Arc<dyn Fn() + Send + Sync> = Arc::from(parts.kill);
        *status.lock() = TerminalStatus::Running { pid: parts.pid };
        let killed = Arc::new(AtomicBool::new(false));
        // The reader thread also writes the replies the application asked for (for example
        // the kitty keyboard query), so the writer is shared.
        let writer = Arc::new(Mutex::new(parts.writer));

        let reader_emulator = emulator.clone();
        let reader_repaint = repaint.clone();
        let reader_writer = writer.clone();
        let reader_status = status.clone();
        let reader_kill = kill.clone();
        let reader = std::thread::Builder::new()
            .name("sqlr-terminal-reader".into())
            .spawn(move || {
                let mut reader = parts.reader;
                let mut buffer = [0u8; 16 * 1024];
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    loop {
                        match reader.read(&mut buffer) {
                            Ok(0) => break,
                            Ok(n) => {
                                reader_emulator.feed(&buffer[..n]);
                                let responses = reader_emulator.take_responses();
                                if !responses.is_empty() {
                                    let mut writer = reader_writer.lock();
                                    let _ = writer.write_all(&responses);
                                    let _ = writer.flush();
                                }
                                reader_repaint();
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                                continue;
                            }
                            Err(_) => break,
                        }
                    }
                }));
                if result.is_err() {
                    *reader_status.lock() = TerminalStatus::Failed {
                        message: "internal panic in terminal reader".into(),
                    };
                    reader_kill();
                    reader_repaint();
                }
            })
            .ok();
        let monitor_status = status.clone();
        let monitor_repaint = repaint;
        let monitor_kill = kill.clone();
        let wait = parts.wait;
        let monitor = std::thread::Builder::new()
            .name("sqlr-terminal-monitor".into())
            .spawn(move || {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(wait)) {
                    Err(_) => {
                        *monitor_status.lock() = TerminalStatus::Failed {
                            message: "internal panic in terminal monitor".into(),
                        };
                        monitor_kill();
                    }
                    Ok(result) => match result {
                        Ok(code) => {
                            let mut status = monitor_status.lock();
                            if !matches!(*status, TerminalStatus::Failed { .. }) {
                                *status = TerminalStatus::Exited { code };
                            }
                        }
                        Err(error) => {
                            *monitor_status.lock() = TerminalStatus::Failed {
                                message: error.to_string(),
                            };
                        }
                    },
                }
                monitor_repaint();
            })
            .ok();
        if reader.is_none() || monitor.is_none() {
            *status.lock() = TerminalStatus::Failed {
                message: "Could not start the terminal worker threads".into(),
            };
            kill();
        }
        Self {
            emulator,
            writer,
            resize_fn: parts.resize,
            kill_fn: kill,
            status,
            reader,
            monitor,
            size: (config.rows, config.cols),
            killed,
        }
    }

    pub fn status(&self) -> TerminalStatus {
        self.status.lock().clone()
    }

    pub fn snapshot(&self, scrollback_offset: usize) -> ScreenSnapshot {
        self.emulator.snapshot(scrollback_offset)
    }

    pub fn feed(&self, bytes: &[u8]) {
        self.emulator.feed(bytes);
    }

    /// Takes the visual bell raised since the last call.
    pub fn take_bell(&self) -> bool {
        self.emulator.take_bell()
    }

    /// The largest scrollback offset [`Self::snapshot`] accepts; `0` is the live screen.
    pub fn scrollback_len(&self) -> usize {
        self.emulator.scrollback_len()
    }

    /// Writes input to the PTY; a dead child ignores it.
    pub fn write(&self, bytes: &[u8]) {
        if !self.status().is_running() {
            return;
        }
        let mut writer = self.writer.lock();
        let _ = writer.write_all(bytes);
        let _ = writer.flush();
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if (rows, cols) == self.size {
            return;
        }
        self.size = (rows, cols);
        self.emulator.resize(rows, cols);
        (self.resize_fn)(rows, cols);
    }

    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    pub fn clear(&self) {
        self.emulator.clear();
    }

    /// Kills the process group after a short grace period. The monitor still reports the
    /// exit code.
    pub fn kill(&self) {
        if !self.status().is_running() {
            return;
        }
        self.killed.store(true, Ordering::Relaxed);
        (self.kill_fn)();
    }

    /// Waits for the status to leave `Running`, polling with a deadline.
    pub fn wait_for_exit(&self, timeout: Duration) -> Option<TerminalStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.status();
            if !status.is_running() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.status.lock().is_running() {
            (self.kill_fn)();
        }
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        output: Vec<u8>,
        exit: u32,
    }

    impl BackendFactory for Fake {
        fn start(
            &self,
            _spec: &CommandSpec,
            _rows: u16,
            _cols: u16,
        ) -> Result<BackendParts, PtyError> {
            struct BrokenReader;
            impl Read for BrokenReader {
                fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                    panic!("reader panic");
                }
            }
            let reader: Box<dyn Read + Send> = if self.output == b"panic-reader" {
                Box::new(BrokenReader)
            } else {
                Box::new(std::io::Cursor::new(self.output.clone()))
            };
            let exit = self.exit;
            Ok(BackendParts {
                reader,
                writer: Box::new(std::io::sink()),
                resize: Box::new(|_, _| {}),
                kill: Box::new(|| {}),
                wait: Box::new(move || {
                    assert_ne!(exit, u32::MAX, "monitor panic");
                    Ok(exit)
                }),
                pid: Some(7),
            })
        }
    }

    #[test]
    fn fake_backend_feeds_the_screen_and_reports_the_exit() {
        let fake = Fake {
            output: b"hello\r\nworld\r\n".to_vec(),
            exit: 3,
        };
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            args: vec![],
            cwd: "/tmp".into(),
        };
        let session =
            TerminalSession::spawn_with(&fake, &spec, &SessionConfig::default(), Arc::new(|| {}));
        let status = session.wait_for_exit(Duration::from_secs(2)).unwrap();
        assert_eq!(status, TerminalStatus::Exited { code: 3 });
        let snapshot = session.snapshot(0);
        assert_eq!(snapshot.text_between((0, 0), (0, 5)), "hello");
        assert_eq!(snapshot.text_between((1, 0), (1, 5)), "world");
    }

    #[test]
    fn a_reader_panic_ends_the_terminal_as_failed() {
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            args: vec![],
            cwd: "/tmp".into(),
        };
        let session = TerminalSession::spawn_with(
            &Fake {
                output: b"panic-reader".to_vec(),
                exit: 0,
            },
            &spec,
            &SessionConfig::default(),
            Arc::new(|| {}),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(session.status(), TerminalStatus::Failed { .. }) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_monitor_panic_ends_the_terminal_as_failed() {
        let spec = CommandSpec {
            program: "/bin/sh".into(),
            args: vec![],
            cwd: "/tmp".into(),
        };
        let session = TerminalSession::spawn_with(
            &Fake {
                output: vec![],
                exit: u32::MAX,
            },
            &spec,
            &SessionConfig::default(),
            Arc::new(|| {}),
        );
        assert!(
            matches!(session.wait_for_exit(Duration::from_secs(2)), Some(TerminalStatus::Failed { message }) if message.contains("internal panic"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn real_pty_runs_a_command_and_keeps_its_output() {
        let config = SessionConfig {
            shell: "printf 'pty works\\n'; exit 4".into(),
            ..SessionConfig::default()
        };
        let session = TerminalSession::spawn(&config, Arc::new(|| {}));
        let status = session
            .wait_for_exit(Duration::from_secs(10))
            .expect("the shell exits");
        assert_eq!(status, TerminalStatus::Exited { code: 4 });
        let snapshot = session.snapshot(0);
        let text = snapshot.text_between((0, 0), (snapshot.rows - 1, snapshot.cols - 1));
        assert!(text.contains("pty works"), "{text:?}");
    }
}
