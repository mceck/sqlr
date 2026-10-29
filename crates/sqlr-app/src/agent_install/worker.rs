//! The background worker: the UI thread sends commands and reads the last snapshot, and
//! never touches an agent's file itself.

use std::sync::{Arc, mpsc};

use parking_lot::Mutex;

use super::{AgentExecutable, AgentKind, AgentRow, Endpoint, Installations};

/// What the UI asks of the worker. Each command carries the endpoint states are computed
/// against.
#[derive(Debug)]
pub enum InstallCommand {
    /// Recompute every state (Settings → Agents opened).
    Refresh(Endpoint),
    /// Install or Update: write sqlr's entry for the running server.
    Install(AgentKind, Endpoint),
    /// Delete sqlr's entry and drop the record.
    Remove(AgentKind, Endpoint),
    /// Rewrite every recorded installation (token rotated, port changed).
    Rewrite(Endpoint),
}

/// The result of an action, for a toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed(AgentKind),
    Removed(AgentKind),
    Failed { kind: AgentKind, error: String },
}

/// The last computed states.
#[derive(Clone, Debug, Default)]
pub struct InstallSnapshot {
    pub rows: Vec<AgentRow>,
    pub executables: Vec<AgentExecutable>,
    /// Commands sent and not yet done.
    pub pending: usize,
}

#[derive(Default)]
struct Shared {
    snapshot: InstallSnapshot,
    outcomes: Vec<InstallOutcome>,
}

/// The handle the app keeps; dropping it stops the worker after its queue.
pub struct AgentInstaller {
    tx: mpsc::Sender<InstallCommand>,
    shared: Arc<Mutex<Shared>>,
}

impl AgentInstaller {
    pub fn spawn(installations: Installations, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, rx) = mpsc::channel::<InstallCommand>();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let worker_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("sqlr-agent-install".into())
            .spawn(move || {
                while let Ok(command) = rx.recv() {
                    run(&installations, command, &worker_shared);
                    repaint();
                }
            });
        if let Err(error) = spawned {
            tracing::error!(error = %error, "the agent installation worker could not start");
        }
        Self { tx, shared }
    }

    pub fn send(&self, command: InstallCommand) {
        self.shared.lock().snapshot.pending += 1;
        if self.tx.send(command).is_err() {
            self.shared.lock().snapshot.pending -= 1;
        }
    }

    pub fn snapshot(&self) -> InstallSnapshot {
        self.shared.lock().snapshot.clone()
    }

    pub fn take_outcomes(&self) -> Vec<InstallOutcome> {
        std::mem::take(&mut self.shared.lock().outcomes)
    }
}

fn run(installations: &Installations, command: InstallCommand, shared: &Mutex<Shared>) {
    let (endpoint, outcome) = match command {
        InstallCommand::Refresh(endpoint) => (endpoint, None),
        InstallCommand::Install(kind, endpoint) => {
            let outcome = match installations.install(kind, &endpoint) {
                Ok(()) => {
                    tracing::info!(agent = kind.id(), "sqlr installed in an agent");
                    InstallOutcome::Installed(kind)
                }
                Err(error) => {
                    tracing::warn!(agent = kind.id(), error = %error, "agent install failed");
                    InstallOutcome::Failed {
                        kind,
                        error: error.to_string(),
                    }
                }
            };
            (endpoint, Some(outcome))
        }
        InstallCommand::Remove(kind, endpoint) => {
            let outcome = match installations.remove(kind) {
                Ok(()) => {
                    tracing::info!(agent = kind.id(), "sqlr removed from an agent");
                    InstallOutcome::Removed(kind)
                }
                Err(error) => {
                    tracing::warn!(agent = kind.id(), error = %error, "agent remove failed");
                    InstallOutcome::Failed {
                        kind,
                        error: error.to_string(),
                    }
                }
            };
            (endpoint, Some(outcome))
        }
        InstallCommand::Rewrite(endpoint) => {
            // The recomputed states show each failure as Outdated (or Manual setup).
            for (kind, error) in installations.rewrite_recorded(&endpoint) {
                tracing::warn!(
                    agent = kind.id(),
                    error = %error,
                    "agent installation could not be rewritten"
                );
            }
            (endpoint, None)
        }
    };
    let rows = installations.rows(&endpoint);
    let executables = installations.env.terminal_executables();
    let mut shared = shared.lock();
    shared.snapshot.rows = rows;
    shared.snapshot.executables = executables;
    shared.snapshot.pending = shared.snapshot.pending.saturating_sub(1);
    shared.outcomes.extend(outcome);
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Arc;

    use parking_lot::Mutex;

    use super::*;
    use crate::agent_install::AgentEnvironment;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_failed_rewrite_is_logged_with_the_agent_id_and_never_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let env = AgentEnvironment::in_home(&home);
        let file = AgentKind::GeminiCli.config_file(&env);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{}").unwrap();
        let installations = Installations::new(env, &dir.path().join("data"));
        let url = "http://127.0.0.1:47821/mcp";
        installations
            .install(AgentKind::GeminiCli, &Endpoint::running(url, "tok-first"))
            .unwrap();
        let parent = file.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500)).unwrap();

        let logs = Captured::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        let shared = Mutex::new(Shared::default());
        tracing::subscriber::with_default(subscriber, || {
            run(
                &installations,
                InstallCommand::Rewrite(Endpoint::running(url, "tok-rotated")),
                &shared,
            );
        });
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();

        let text = String::from_utf8(logs.0.lock().clone()).unwrap();
        let line = text
            .lines()
            .find(|line| line.contains("could not be rewritten"))
            .unwrap_or_else(|| panic!("no rewrite failure logged: {text}"));
        assert!(line.contains("WARN"), "{line}");
        assert!(line.contains("agent=\"gemini-cli\""), "{line}");
        assert!(!text.contains("tok-rotated"), "{text}");
        assert_eq!(text.matches("could not be rewritten").count(), 1, "{text}");
    }
}
