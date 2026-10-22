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
