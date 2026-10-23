//! `connections.toml` and the keychain, read and written off the UI thread: the app sends
//! [`StoreOp`]s to one worker thread, which runs them in order, and polls the
//! [`StoreReply`]s every frame. Every change is answered with the saved connections and
//! folders as they now are.

use std::sync::mpsc;

use sqlr_core::{ConnectionConfig, DriverManifest, Secrets};
use sqlr_store::{ConnectionRepo, MoveTarget};

/// The saved connections and folders, or why they could not be read.
pub(crate) struct Listing {
    pub connections: Result<Vec<ConnectionConfig>, String>,
    pub folders: Result<Vec<String>, String>,
}

impl Listing {
    pub(crate) fn read(repo: &ConnectionRepo) -> Self {
        Self {
            connections: repo.list().map_err(|e| e.to_string()),
            folders: repo.folders().map_err(|e| e.to_string()),
        }
    }
}

/// A request to the store. No `Debug`: `Save` carries secrets.
pub(crate) enum StoreOp {
    List,
    CreateFolder(String),
    RenameFolder {
        from: String,
        to: String,
    },
    DeleteFolder(String),
    MoveConnection {
        conn: String,
        folder: Option<String>,
        target: MoveTarget,
    },
    MoveFolder {
        folder: String,
        target: MoveTarget,
    },
    /// The connection dialog's Save; `editing` when it edited a saved connection.
    Save {
        cfg: ConnectionConfig,
        secrets: Secrets,
        editing: bool,
    },
    Delete {
        cfg: ConnectionConfig,
        manifest: DriverManifest,
    },
    /// Saves a copy of `cfg` (with its stored secrets when it saves them) right after it.
    Duplicate {
        cfg: ConnectionConfig,
        manifest: DriverManifest,
    },
    /// The stored secrets of `cfg`, for the connection dialog.
    SecretsFor {
        cfg: ConnectionConfig,
        manifest: DriverManifest,
    },
}

/// What a [`StoreOp`] did. `Err` holds the store error's text.
pub(crate) enum StoreReply {
    Listed(Listing),
    FolderCreated {
        name: String,
        result: Result<(), String>,
        listing: Listing,
    },
    FolderRenamed {
        from: String,
        to: String,
        result: Result<(), String>,
        listing: Listing,
    },
    FolderDeleted {
        name: String,
        result: Result<(), String>,
        listing: Listing,
    },
    ConnectionMoved {
        conn: String,
        folder: Option<String>,
        result: Result<(), String>,
        listing: Listing,
    },
    FolderMoved {
        folder: String,
        /// The folder's path after the move (its leaf name may have changed with its parent).
        to: String,
        result: Result<(), String>,
        listing: Listing,
    },
    Saved {
        id: String,
        editing: bool,
        result: Result<(), String>,
        listing: Listing,
    },
    Deleted {
        id: String,
        name: String,
        result: Result<(), String>,
        listing: Listing,
    },
    Secrets {
        id: String,
        result: Result<Secrets, String>,
    },
    /// `copy` is the duplicate's id.
    Duplicated {
        name: String,
        copy: Result<String, String>,
        listing: Listing,
    },
}

/// The store's worker thread and its queues.
pub(crate) struct StoreWorker {
    ops: mpsc::Sender<StoreOp>,
    replies: mpsc::Receiver<StoreReply>,
    /// Ops sent and not answered yet.
    in_flight: usize,
}

impl StoreWorker {
    /// Starts the worker; `wake` runs after every reply (the UI repaints).
    pub(crate) fn spawn(repo: ConnectionRepo, wake: impl Fn() + Send + 'static) -> Self {
        let (ops, op_rx) = mpsc::channel::<StoreOp>();
        let (reply_tx, replies) = mpsc::channel();
        std::thread::Builder::new()
            .name("sqlr-store".into())
            .spawn(move || {
                for op in op_rx {
                    let reply = run(&repo, op);
                    if reply_tx.send(reply).is_err() {
                        return;
                    }
                    wake();
                }
            })
            .expect("spawn the store thread");
        Self {
            ops,
            replies,
            in_flight: 0,
        }
    }

    pub(crate) fn send(&mut self, op: StoreOp) {
        if self.ops.send(op).is_ok() {
            self.in_flight += 1;
        }
    }

    /// The next reply, if one arrived.
    pub(crate) fn try_recv(&mut self) -> Option<StoreReply> {
        let reply = self.replies.try_recv().ok()?;
        self.in_flight = self.in_flight.saturating_sub(1);
        Some(reply)
    }

    /// Whether ops are still on their way.
    #[cfg(test)]
    pub(crate) fn busy(&self) -> bool {
        self.in_flight > 0
    }
}

/// Runs `op` on `repo` (on the store thread; tests call it in place).
pub(crate) fn run(repo: &ConnectionRepo, op: StoreOp) -> StoreReply {
    let text = |r: Result<(), sqlr_store::StoreError>| r.map_err(|e| e.to_string());
    match op {
        StoreOp::List => StoreReply::Listed(Listing::read(repo)),
        StoreOp::CreateFolder(name) => {
            let (name, result) = match repo.create_folder(&name) {
                Ok(name) => (name, Ok(())),
                Err(e) => (name, Err(e.to_string())),
            };
            StoreReply::FolderCreated {
                name,
                result,
                listing: Listing::read(repo),
            }
        }
        StoreOp::RenameFolder { from, to } => {
            let (to, result) = match repo.rename_folder(&from, &to) {
                Ok(to) => (to, Ok(())),
                Err(e) => (to, Err(e.to_string())),
            };
            StoreReply::FolderRenamed {
                from,
                to,
                result,
                listing: Listing::read(repo),
            }
        }
        StoreOp::DeleteFolder(name) => {
            let result = text(repo.delete_folder(&name));
            StoreReply::FolderDeleted {
                name,
                result,
                listing: Listing::read(repo),
            }
        }
        StoreOp::MoveConnection {
            conn,
            folder,
            target,
        } => {
            let result = text(repo.move_connection(&conn, folder.as_deref(), target));
            StoreReply::ConnectionMoved {
                conn,
                folder,
                result,
                listing: Listing::read(repo),
            }
        }
        StoreOp::MoveFolder { folder, target } => {
            let (result, to) = match repo.move_folder(&folder, target) {
                Ok(to) => (Ok(()), to),
                Err(e) => (Err(e.to_string()), String::new()),
            };
            StoreReply::FolderMoved {
                folder,
                to,
                result,
                listing: Listing::read(repo),
            }
        }
        StoreOp::Save {
            cfg,
            secrets,
            editing,
        } => {
            let result = text(repo.save(&cfg, &secrets));
            StoreReply::Saved {
                id: cfg.id,
                editing,
                result,
                listing: Listing::read(repo),
            }
        }
        StoreOp::Delete { cfg, manifest } => {
            let result = text(repo.delete(&cfg.id, &manifest));
            StoreReply::Deleted {
                id: cfg.id,
                name: cfg.name,
                result,
                listing: Listing::read(repo),
            }
        }
        StoreOp::Duplicate { cfg, manifest } => StoreReply::Duplicated {
            copy: duplicate(repo, &cfg, &manifest).map_err(|e| e.to_string()),
            name: cfg.name,
            listing: Listing::read(repo),
        },
        StoreOp::SecretsFor { cfg, manifest } => StoreReply::Secrets {
            result: repo.secrets_for(&cfg, &manifest).map_err(|e| e.to_string()),
            id: cfg.id,
        },
    }
}

/// Saves a copy of `cfg` under a free name, with its stored secrets, right after it; returns
/// the copy's id.
fn duplicate(
    repo: &ConnectionRepo,
    cfg: &ConnectionConfig,
    manifest: &DriverManifest,
) -> Result<String, sqlr_store::StoreError> {
    let secrets = if cfg.save_secrets {
        repo.secrets_for(cfg, manifest).inspect_err(|e| {
            tracing::warn!(conn = %cfg.id, error = %e, "reading secrets failed");
        })?
    } else {
        Secrets::default()
    };
    let mut copy = cfg.clone();
    copy.id = uuid::Uuid::new_v4().to_string();
    let saved = repo.list()?;
    let taken: Vec<&str> = saved.iter().map(|c| c.name.as_str()).collect();
    copy.name = crate::dialogs::copy_name(&cfg.name, &taken);
    repo.save(&copy, &secrets).inspect_err(|e| {
        tracing::warn!(conn = %copy.id, error = %e, "duplicating the connection failed");
    })?;
    // `save` appends; put the copy right after the original.
    let after = MoveTarget::After(cfg.id.clone());
    if let Err(e) = repo.move_connection(&copy.id, copy.group.as_deref(), after) {
        tracing::warn!(conn = %copy.id, error = %e, "placing the duplicated connection failed");
    }
    Ok(copy.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use sqlr_core::Driver;
    use sqlr_store::{ConnectionRepo, MemorySecretStore};
    use sqlr_testkit::FakeDriver;

    fn repo(dir: &std::path::Path) -> ConnectionRepo {
        ConnectionRepo::open(dir, Box::new(MemorySecretStore::default())).unwrap()
    }

    fn config(id: &str, name: &str) -> ConnectionConfig {
        ConnectionConfig {
            id: id.into(),
            name: name.into(),
            driver: "fake".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: true,
        }
    }

    fn manifest() -> DriverManifest {
        FakeDriver::new(Default::default()).manifest().clone()
    }

    #[test]
    fn every_store_operation_answers_with_a_listing() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo(dir.path());
        let cfg = config("c1", "one");
        assert!(matches!(
            run(&repo, StoreOp::Save {
                cfg: cfg.clone(),
                secrets: Secrets::default(),
                editing: false,
            }),
            StoreReply::Saved { ref id, editing: false, result: Ok(()), listing } if id == "c1" && listing.connections.as_ref().is_ok_and(|c| c.len() == 1)
        ));
        assert!(matches!(
            run(&repo, StoreOp::List),
            StoreReply::Listed(Listing { connections: Ok(c), .. }) if c.len() == 1
        ));
        assert!(matches!(
            run(&repo, StoreOp::CreateFolder("work".into())),
            StoreReply::FolderCreated { result: Ok(()), .. }
        ));
        assert!(matches!(
            run(&repo, StoreOp::RenameFolder { from: "work".into(), to: "home".into() }),
            StoreReply::FolderRenamed { to, result: Ok(()), .. } if to == "home"
        ));
        assert!(matches!(
            run(
                &repo,
                StoreOp::MoveConnection {
                    conn: "c1".into(),
                    folder: Some("home".into()),
                    target: MoveTarget::Into("home".into()),
                }
            ),
            StoreReply::ConnectionMoved { result: Ok(()), .. }
        ));
        assert!(matches!(
            run(&repo, StoreOp::MoveFolder {
                folder: "home".into(),
                target: MoveTarget::End,
            }),
            StoreReply::FolderMoved { folder, result: Ok(()), to, .. } if folder == "home" && to == "home"
        ));
        assert!(matches!(
            run(&repo, StoreOp::DeleteFolder("home".into())),
            StoreReply::FolderDeleted { result: Ok(()), .. }
        ));
        assert!(matches!(
            run(&repo, StoreOp::SecretsFor {
                cfg: cfg.clone(),
                manifest: manifest(),
            }),
            StoreReply::Secrets { ref id, result: Ok(_) } if id == "c1"
        ));
        assert!(matches!(
            run(&repo, StoreOp::Duplicate {
                cfg: cfg.clone(),
                manifest: manifest(),
            }),
            StoreReply::Duplicated { copy: Ok(_), listing, .. } if listing.connections.as_ref().is_ok_and(|c| c.len() == 2)
        ));
        assert!(matches!(
            run(&repo, StoreOp::Delete {
                cfg,
                manifest: manifest(),
            }),
            StoreReply::Deleted { result: Ok(()), listing, .. } if listing.connections.as_ref().is_ok_and(|c| c.len() == 1)
        ));
    }

    #[test]
    fn failing_operations_report_the_store_error() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo(dir.path());
        assert!(matches!(
            run(
                &repo,
                StoreOp::RenameFolder {
                    from: "missing".into(),
                    to: "x".into()
                }
            ),
            StoreReply::FolderRenamed { result: Err(_), .. }
        ));
        assert!(matches!(
            run(&repo, StoreOp::DeleteFolder("missing".into())),
            StoreReply::FolderDeleted { result: Ok(()), .. }
        ));
        assert!(matches!(
            run(&repo, StoreOp::MoveFolder {
                folder: "missing".into(),
                target: MoveTarget::End,
            }),
            StoreReply::FolderMoved { result: Err(_), to, .. } if to.is_empty()
        ));
        assert!(matches!(
            run(
                &repo,
                StoreOp::MoveConnection {
                    conn: "missing".into(),
                    folder: None,
                    target: MoveTarget::End,
                }
            ),
            StoreReply::ConnectionMoved { result: Err(_), .. }
        ));
    }

    #[test]
    fn the_worker_runs_ops_in_order_and_counts_them() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo(dir.path());
        let woke = Arc::new(AtomicBool::new(false));
        let flag = woke.clone();
        let mut worker = StoreWorker::spawn(repo, move || {
            flag.store(true, Ordering::SeqCst);
        });
        assert!(!worker.busy());
        worker.send(StoreOp::List);
        assert!(worker.busy());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(reply) = worker.try_recv() {
                assert!(matches!(reply, StoreReply::Listed(_)));
                break;
            }
            assert!(Instant::now() < deadline, "the store worker never answered");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!worker.busy());
        assert!(woke.load(Ordering::SeqCst));
        assert!(worker.try_recv().is_none());
    }
}
