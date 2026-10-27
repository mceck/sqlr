//! The background worker: the UI thread sends commands and reads the last snapshot, and
//! never touches an agent's file itself.

use std::sync::{Arc, mpsc};

use parking_lot::Mutex;

use super::{AgentKind, AgentRow, Endpoint, Installations};

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
    installations: Installations,
}

impl AgentInstaller {
    pub fn spawn(installations: Installations, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, rx) = mpsc::channel::<InstallCommand>();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let worker_shared = shared.clone();
        let worker = installations.clone();
        let spawned = std::thread::Builder::new()
            .name("sqlr-agent-install".into())
            .spawn(move || {
                while let Ok(command) = rx.recv() {
                    run(&worker, command, &worker_shared);
                    repaint();
                }
            });
        if let Err(error) = spawned {
            tracing::error!(error = %error, "the agent installation worker could not start");
        }
        Self {
            tx,
            shared,
            installations,
        }
    }

    pub fn installations(&self) -> &Installations {
        &self.installations
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
                    tracing::info!(agent = kind.name(), "sqlr installed in an agent");
                    InstallOutcome::Installed(kind)
                }
                Err(error) => {
                    tracing::warn!(agent = kind.name(), error = %error, "agent install failed");
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
                    tracing::info!(agent = kind.name(), "sqlr removed from an agent");
                    InstallOutcome::Removed(kind)
                }
                Err(error) => {
                    tracing::warn!(agent = kind.name(), error = %error, "agent remove failed");
                    InstallOutcome::Failed {
                        kind,
                        error: error.to_string(),
                    }
                }
            };
            (endpoint, Some(outcome))
        }
        InstallCommand::Rewrite(endpoint) => {
            // Failures are logged inside; the recomputed state shows them as Outdated.
            let _ = installations.rewrite_recorded(&endpoint);
            (endpoint, None)
        }
    };
    let rows = installations.rows(&endpoint);
    let mut shared = shared.lock();
    shared.snapshot.rows = rows;
    shared.snapshot.pending = shared.snapshot.pending.saturating_sub(1);
    shared.outcomes.extend(outcome);
}
