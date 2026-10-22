//! Keychain secrets and connection profiles, read and written off the UI thread: secrets on
//! a session's connect, profiles on the engine's sequential `sqlr.db` worker.

use std::path::PathBuf;
use std::sync::Arc;

use sqlr_core::{ConnectionConfig, ConnectionProfile, DriverManifest, Secrets};
use sqlr_store::{ProfileRepo, SecretStore, StoreError};

use crate::engine::Shared;
use crate::ssh::with_ssh_fields;

/// The profiles as last loaded, and a revision bumped on every change.
#[derive(Default)]
pub(crate) struct ProfilesState {
    pub revision: u64,
    pub list: Option<Arc<Vec<ConnectionProfile>>>,
}

impl Shared {
    /// The secrets a session connects with: those given to `connect` (entered by the user),
    /// then, for a connection that saves its secrets, the keychain's for every other secret
    /// field (the SSH ones included). Read once per `connect`; tab sessions reuse them.
    pub(crate) async fn connect_secrets(
        &self,
        cfg: &ConnectionConfig,
        manifest: &DriverManifest,
        given: Secrets,
    ) -> Secrets {
        let store = self.secret_store.lock().clone();
        let Some(store) = store.filter(|_| cfg.save_secrets) else {
            return given;
        };
        if let Some(cached) = self.cached_secrets(cfg, &given) {
            return cached;
        }
        let missing: Vec<String> = with_ssh_fields(manifest.clone())
            .fields
            .into_iter()
            .filter(|f| f.secret && given.0.get(&f.key).is_none_or(String::is_empty))
            .map(|f| f.key)
            .collect();
        let id = cfg.id.clone();
        let read = tokio::task::spawn_blocking(move || {
            let mut found = Vec::new();
            for key in missing {
                if let Some(value) = store.get(&id, &key)? {
                    found.push((key, value));
                }
            }
            Ok::<_, StoreError>(found)
        })
        .await;
        let mut secrets = given.clone();
        match read {
            Ok(Ok(found)) => secrets.0.extend(found),
            Ok(Err(e)) => self.notice(format!("Could not read secrets for {}: {e}", cfg.name)),
            Err(e) => self.notice(format!("Could not read secrets for {}: {e}", cfg.name)),
        }
        self.cache_secrets(cfg, &given, &secrets);
        secrets
    }

    /// Reads the profiles (built-ins with overrides, then user profiles) from `dir`.
    pub(crate) fn load_profiles(self: &Arc<Self>, dir: PathBuf) {
        self.profile_job(dir, "read", |_| Ok(()));
    }

    /// Saves `p` (an override for a built-in id) to `dir`, then reloads.
    pub(crate) fn save_profile(self: &Arc<Self>, dir: PathBuf, p: ConnectionProfile) {
        self.profile_job(dir, "save", move |repo| repo.save(&p));
    }

    /// Deletes the profile `id` (a built-in's override) from `dir`, then reloads.
    pub(crate) fn delete_profile(self: &Arc<Self>, dir: PathBuf, id: String) {
        self.profile_job(dir, "delete", move |repo| repo.delete(&id));
    }

    /// Runs `op` and a reload on the sequential `sqlr.db` worker, so profile writes land in
    /// the order they were asked for.
    fn profile_job(
        self: &Arc<Self>,
        dir: PathBuf,
        what: &'static str,
        op: impl FnOnce(&ProfileRepo) -> Result<(), StoreError> + Send + 'static,
    ) {
        let weak = Arc::downgrade(self);
        self.db.push(move |_| {
            let repo = ProfileRepo::open(&dir);
            let result = op(&repo).and_then(|()| repo.list());
            let Some(shared) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(list) => {
                    {
                        let mut st = shared.profiles.lock();
                        st.revision += 1;
                        st.list = Some(Arc::new(list));
                    }
                    shared.repaint();
                }
                Err(e) => shared.notice(format!("Could not {what} connection profiles: {e}")),
            }
        });
    }
}

/// The `Arc` form the engine keeps of a secret store.
pub(crate) type SharedSecretStore = Arc<dyn SecretStore>;
