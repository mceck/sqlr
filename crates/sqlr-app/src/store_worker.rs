//! `connections.toml` and the keychain, read and written off the UI thread: the app sends
//! [`StoreOp`]s to one worker thread, which runs them in order, and polls the
//! [`StoreReply`]s every frame. Every change is answered with the saved connections and
//! folders as they now are.

use std::sync::mpsc;

use sqlr_core::{ConnectionConfig, DriverManifest, Secrets};
use sqlr_store::client_migration::{self, ClientFormat, DetectedSource, ImportBatch};
use sqlr_store::{ConnectionRepo, MoveTarget};

pub(crate) enum MigrationRequest {
    Discover,
    Read(std::path::PathBuf),
    Clipboard(String),
    Import(Vec<(ConnectionConfig, Secrets)>),
    Export {
        path: std::path::PathBuf,
        format: ClientFormat,
        connections: Vec<ConnectionConfig>,
        secret_fields: Option<std::collections::BTreeMap<String, Vec<String>>>,
    },
}

pub(crate) enum MigrationResult {
    Sources {
        sources: Vec<DetectedSource>,
        batch: ImportBatch,
    },
    Loaded {
        format: ClientFormat,
        batch: ImportBatch,
    },
    Imported(usize),
    Exported {
        path: std::path::PathBuf,
        warnings: Vec<String>,
    },
    Error(String),
}

/// The saved connections and folders, or why they could not be read.
pub(crate) struct Listing {
    pub connections: Result<Vec<ConnectionConfig>, String>,
    pub folders: Result<Vec<String>, String>,
}

impl Listing {
    pub(crate) fn read(repo: &ConnectionRepo) -> Self {
        Self {
            connections: repo.list().map_err(store_error),
            folders: repo.folders().map_err(store_error),
        }
    }
}

/// A request to the store. No `Debug`: `Save` carries secrets.
pub(crate) enum StoreOp {
    ClientMigration {
        id: String,
        request: MigrationRequest,
    },
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
    /// Changes only the exposure of a saved connection, without reconnecting it.
    SetAgentAccess {
        id: String,
        access: sqlr_core::AgentAccess,
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
    ClientMigration {
        id: String,
        result: MigrationResult,
        listing: Option<Listing>,
    },
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

impl StoreOp {
    fn panic_reply(&self) -> StoreReply {
        let error = || "internal panic in the store worker".to_string();
        let listing = || Listing {
            connections: Err(error()),
            folders: Err(error()),
        };
        match self {
            Self::ClientMigration { id, .. } => StoreReply::ClientMigration {
                id: id.clone(),
                result: MigrationResult::Error(error()),
                listing: None,
            },
            Self::List => StoreReply::Listed(listing()),
            Self::CreateFolder(name) => StoreReply::FolderCreated {
                name: name.clone(),
                result: Err(error()),
                listing: listing(),
            },
            Self::RenameFolder { from, to } => StoreReply::FolderRenamed {
                from: from.clone(),
                to: to.clone(),
                result: Err(error()),
                listing: listing(),
            },
            Self::DeleteFolder(name) => StoreReply::FolderDeleted {
                name: name.clone(),
                result: Err(error()),
                listing: listing(),
            },
            Self::MoveConnection { conn, folder, .. } => StoreReply::ConnectionMoved {
                conn: conn.clone(),
                folder: folder.clone(),
                result: Err(error()),
                listing: listing(),
            },
            Self::MoveFolder { folder, .. } => StoreReply::FolderMoved {
                folder: folder.clone(),
                to: folder.clone(),
                result: Err(error()),
                listing: listing(),
            },
            Self::Save { cfg, editing, .. } => StoreReply::Saved {
                id: cfg.id.clone(),
                editing: *editing,
                result: Err(error()),
                listing: listing(),
            },
            Self::SetAgentAccess { id, .. } => StoreReply::Saved {
                id: id.clone(),
                editing: false,
                result: Err(error()),
                listing: listing(),
            },
            Self::Delete { cfg, .. } => StoreReply::Deleted {
                id: cfg.id.clone(),
                name: cfg.name.clone(),
                result: Err(error()),
                listing: listing(),
            },
            Self::Duplicate { cfg, .. } => StoreReply::Duplicated {
                name: cfg.name.clone(),
                copy: Err(error()),
                listing: listing(),
            },
            Self::SecretsFor { cfg, .. } => StoreReply::Secrets {
                id: cfg.id.clone(),
                result: Err(error()),
            },
        }
    }
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
                    let fallback = op.panic_reply();
                    let reply =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&repo, op)))
                            .unwrap_or(fallback);
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

fn store_error(error: sqlr_store::StoreError) -> String {
    if !matches!(error, sqlr_store::StoreError::Invalid(_)) {
        tracing::error!(op = "store", error = %error, "local storage operation failed");
    }
    error.to_string()
}

/// Runs `op` on `repo` (on the store thread; tests call it in place).
pub(crate) fn run(repo: &ConnectionRepo, op: StoreOp) -> StoreReply {
    let text = |r: Result<(), sqlr_store::StoreError>| r.map_err(store_error);
    match op {
        StoreOp::ClientMigration { id, request } => {
            let importing = matches!(&request, MigrationRequest::Import(_));
            let result = migrate(repo, request).unwrap_or_else(MigrationResult::Error);
            StoreReply::ClientMigration {
                id,
                result,
                listing: importing.then(|| Listing::read(repo)),
            }
        }
        StoreOp::List => StoreReply::Listed(Listing::read(repo)),
        StoreOp::CreateFolder(name) => {
            let (name, result) = match repo.create_folder(&name) {
                Ok(name) => (name, Ok(())),
                Err(e) => (name, Err(store_error(e))),
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
                Err(e) => (to, Err(store_error(e))),
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
                Err(e) => (Err(store_error(e)), String::new()),
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
        StoreOp::SetAgentAccess { id, access } => {
            let result = repo
                .list()
                .and_then(|list| {
                    let mut cfg = list.into_iter().find(|cfg| cfg.id == id).ok_or_else(|| {
                        sqlr_store::StoreError::Invalid("unknown connection".into())
                    })?;
                    cfg.agent_access = access;
                    repo.save(&cfg, &Secrets::default())
                })
                .map_err(store_error);
            StoreReply::Saved {
                id,
                editing: false,
                result,
                listing: Listing::read(repo),
            }
        }
        StoreOp::Duplicate { cfg, manifest } => StoreReply::Duplicated {
            copy: duplicate(repo, &cfg, &manifest).map_err(store_error),
            name: cfg.name,
            listing: Listing::read(repo),
        },
        StoreOp::SecretsFor { cfg, manifest } => StoreReply::Secrets {
            result: repo.secrets_for(&cfg, &manifest).map_err(store_error),
            id: cfg.id,
        },
    }
}

fn migrate(repo: &ConnectionRepo, request: MigrationRequest) -> Result<MigrationResult, String> {
    match request {
        MigrationRequest::Discover => {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(std::path::PathBuf::from)
                .ok_or("Could not locate your home folder. Choose a connection file instead.")?;
            let sources = client_migration::discover(&home);
            let mut combined = ImportBatch::default();
            for source in &sources {
                match client_migration::read_source(&source.path) {
                    Ok((_, mut batch)) => {
                        let offset = combined.connections.len();
                        combined
                            .credentials
                            .extend(batch.credentials.into_iter().map(|(i, s)| (i + offset, s)));
                        combined.connections.append(&mut batch.connections);
                        combined.warnings.append(&mut batch.warnings);
                    }
                    Err(e) => combined
                        .warnings
                        .push(format!("{}: {e}", source.path.display())),
                }
            }
            Ok(MigrationResult::Sources {
                sources,
                batch: combined,
            })
        }
        MigrationRequest::Read(path) => {
            let (format, batch) = client_migration::read_source(&path)?;
            Ok(MigrationResult::Loaded { format, batch })
        }
        MigrationRequest::Clipboard(text) => {
            if text.len() > 16 * 1024 * 1024 {
                return Err("Clipboard contents exceed the 16 MiB limit.".into());
            }
            let format = client_migration::detect(text.as_bytes())?;
            let batch = client_migration::decode(format, text.as_bytes())?;
            Ok(MigrationResult::Loaded { format, batch })
        }
        MigrationRequest::Import(mut connections) => {
            for (c, _) in &mut connections {
                c.id = uuid::Uuid::new_v4().to_string();
            }
            let configs: Vec<_> = connections.iter().map(|(c, _)| c.clone()).collect();
            let count = repo.import_connections(&configs).map_err(store_error)?;
            let saved = repo.list().map_err(store_error)?;
            for (c, secrets) in connections {
                if !secrets.0.is_empty()
                    && let Some(mut cfg) = saved.iter().find(|saved| saved.id == c.id).cloned()
                {
                    cfg.save_secrets = true;
                    repo.save(&cfg, &secrets).map_err(store_error)?;
                }
            }
            Ok(MigrationResult::Imported(count))
        }
        MigrationRequest::Export {
            path,
            format,
            connections,
            secret_fields,
        } => {
            let warnings = if let Some(fields) = secret_fields {
                let mut credentials = std::collections::BTreeMap::new();
                for c in &connections {
                    let mut secrets = Secrets::default();
                    let keys = fields.get(&c.id).ok_or("Cannot export credentials without the connection's driver manifest. Install the driver first.")?;
                    for key in keys.iter().filter(|_| c.save_secrets) {
                        if let Some(value) =
                            repo.secret_store().get(&c.id, key).map_err(store_error)?
                        {
                            secrets.0.insert(key.clone(), value);
                        }
                    }
                    credentials.insert(c.id.clone(), secrets);
                }
                let doc =
                    client_migration::encode_with_credentials(format, &connections, &credentials)?;
                client_migration::save_document(&path, doc)?
            } else {
                client_migration::save_export(&path, format, &connections)?
            };
            Ok(MigrationResult::Exported { path, warnings })
        }
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
    // Consent never travels with a copy.
    copy.agent_access = sqlr_core::AgentAccess::None;
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
            agent_access: Default::default(),
        }
    }

    fn manifest() -> DriverManifest {
        FakeDriver::new(Default::default()).manifest().clone()
    }

    #[test]
    fn credential_export_and_import_keep_secrets_out_of_connection_files() {
        let source_dir = tempfile::tempdir().unwrap();
        let source = repo(source_dir.path());
        let mut c = config("source", "Sales");
        c.driver = "postgres".into();
        c.values.insert("host".into(), "db.example".into());
        let secrets = Secrets(
            [
                ("password".into(), "database-secret".into()),
                ("ssh_password".into(), "tunnel-secret".into()),
            ]
            .into(),
        );
        source.save(&c, &secrets).unwrap();
        let path = source_dir.path().join("export.json");
        migrate(
            &source,
            MigrationRequest::Export {
                path: path.clone(),
                format: ClientFormat::Sqlr,
                connections: vec![c.clone()],
                secret_fields: None,
            },
        )
        .unwrap();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("database-secret")
        );
        migrate(
            &source,
            MigrationRequest::Export {
                path: path.clone(),
                format: ClientFormat::Sqlr,
                connections: vec![c.clone()],
                secret_fields: Some(
                    [(c.id.clone(), vec!["password".into(), "ssh_password".into()])].into(),
                ),
            },
        )
        .unwrap();
        let (_, batch) = client_migration::read_source(&path).unwrap();
        assert!(batch.connections[0].id.is_empty());
        let destination_dir = tempfile::tempdir().unwrap();
        let destination = repo(destination_dir.path());
        let entries = batch
            .connections
            .into_iter()
            .enumerate()
            .map(|(i, cfg)| (cfg, batch.credentials[&i].clone()))
            .collect();
        migrate(&destination, MigrationRequest::Import(entries)).unwrap();
        let saved = destination.list().unwrap();
        assert!(saved[0].save_secrets);
        assert_eq!(
            destination
                .secret_store()
                .get(&saved[0].id, "password")
                .unwrap()
                .as_deref(),
            Some("database-secret")
        );
        assert_eq!(
            destination
                .secret_store()
                .get(&saved[0].id, "ssh_password")
                .unwrap()
                .as_deref(),
            Some("tunnel-secret")
        );
        let toml =
            std::fs::read_to_string(destination_dir.path().join("connections.toml")).unwrap();
        assert!(!toml.contains("database-secret"));
        assert!(!toml.contains("tunnel-secret"));
        migrate(
            &destination,
            MigrationRequest::Import(vec![(
                c,
                Secrets([("password".into(), "replacement".into())].into()),
            )]),
        )
        .unwrap();
        assert_eq!(destination.list().unwrap().len(), 1);
        assert_eq!(
            destination
                .secret_store()
                .get(&saved[0].id, "password")
                .unwrap()
                .as_deref(),
            Some("database-secret")
        );
    }

    #[test]
    fn a_panicking_store_operation_fails_and_the_worker_keeps_answering() {
        struct BrokenSecrets;
        impl sqlr_store::SecretStore for BrokenSecrets {
            fn get(&self, _: &str, _: &str) -> Result<Option<String>, sqlr_store::StoreError> {
                panic!("broken keychain");
            }
            fn set(&self, _: &str, _: &str, _: &str) -> Result<(), sqlr_store::StoreError> {
                panic!("broken keychain");
            }
            fn delete(&self, _: &str, _: &str) -> Result<(), sqlr_store::StoreError> {
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = ConnectionRepo::open(dir.path(), Box::new(BrokenSecrets)).unwrap();
        let mut worker = StoreWorker::spawn(repo, || {});
        worker.send(StoreOp::Save {
            cfg: config("c", "test"),
            secrets: Secrets([("password".into(), "sentinel".into())].into()),
            editing: false,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(StoreReply::Saved {
                result: Err(error), ..
            }) = worker.try_recv()
            {
                assert!(error.contains("internal panic"));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the panicking operation must answer"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        worker.send(StoreOp::List);
        loop {
            if matches!(worker.try_recv(), Some(StoreReply::Listed(_))) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the next operation must answer"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn duplicating_a_connection_resets_agent_access() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo(dir.path());
        let mut cfg = config("c1", "one");
        cfg.agent_access = sqlr_core::AgentAccess::Write;
        let manifest = manifest();
        repo.save(&cfg, &Secrets::default()).unwrap();
        let copy = duplicate(&repo, &cfg, &manifest).unwrap();
        let saved = repo.list().unwrap();
        let copy = saved.iter().find(|config| config.id == copy).unwrap();
        assert_eq!(copy.agent_access, sqlr_core::AgentAccess::None);
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
