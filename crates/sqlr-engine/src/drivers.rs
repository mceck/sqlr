//! Drivers that do not ship with the app: the registry index, installs,
//! updates and rollbacks, manual installs, and the sidecars registered from the install
//! directory at startup and after every change, with no restart.
//!
//! Every method returns immediately; a job runs on the engine runtime and the UI reads the
//! state each frame. Nothing is fetched until a caller asks (spec §1.2: no background
//! update checks).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use sqlr_registry::{
    DEFAULT_INDEX_URL, InstalledDriver, ManualInspection, REGISTRY_PUBLIC_KEY, Registry,
    RegistryEntry,
};
use sqlr_sidecar::{SidecarDriver, SidecarSpec};

use crate::engine::{Command, Engine, Shared};
use crate::registry::DriverRegistry;

/// Where an install, update or removal stands.
#[derive(Clone, Debug, PartialEq)]
pub enum DriverJobState {
    Running { done: u64, total: Option<u64> },
    Done,
    Failed(String),
}

/// The registry index as last fetched.
#[derive(Clone, Debug, PartialEq)]
pub enum IndexState {
    /// Not fetched yet: nothing is fetched until the Driver Manager asks.
    Idle,
    Loading,
    Ready(Arc<Vec<RegistryEntry>>),
    Failed(String),
}

/// An install from a file the user picked: first inspected, then confirmed.
#[derive(Clone, Debug, PartialEq)]
pub enum ManualState {
    None,
    Inspecting,
    /// The file's id, version and checksum, waiting for "Install anyway".
    Review(ManualInspection),
    Failed(String),
}

struct Hub {
    registry: Option<Arc<Registry>>,
    idle: Duration,
    index: IndexState,
    installed: Vec<InstalledDriver>,
    jobs: BTreeMap<String, DriverJobState>,
    manual: ManualState,
    /// The sidecars this hub registered, to restart them.
    sidecars: HashMap<String, Arc<SidecarDriver>>,
}

pub(crate) struct DriverHub {
    inner: Mutex<Hub>,
    drivers: DriverRegistry,
    revision: AtomicU64,
    loaded: AtomicBool,
}

impl DriverHub {
    pub(crate) fn new(drivers: DriverRegistry) -> Self {
        Self {
            inner: Mutex::new(Hub {
                registry: None,
                idle: Duration::from_secs(300),
                index: IndexState::Idle,
                installed: Vec::new(),
                jobs: BTreeMap::new(),
                manual: ManualState::None,
                sidecars: HashMap::new(),
            }),
            drivers,
            revision: AtomicU64::new(0),
            loaded: AtomicBool::new(false),
        }
    }

    fn bump(&self, shared: &Shared) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        shared.repaint();
    }

    fn registry(&self) -> Option<Arc<Registry>> {
        self.inner.lock().registry.clone()
    }

    /// Registers the active version of every installed driver (a bundled driver with the
    /// same id wins) and forgets the sidecars of drivers that are gone.
    fn sync_drivers(&self, installed: &[InstalledDriver]) {
        let mut hub = self.inner.lock();
        let idle = hub.idle;
        let gone: Vec<String> = hub
            .sidecars
            .keys()
            .filter(|id| !installed.iter().any(|d| &d.id == *id))
            .cloned()
            .collect();
        for id in gone {
            if let Some(old) = hub.sidecars.remove(&id) {
                old.restart();
            }
            self.drivers.unregister(&id);
        }
        for driver in installed {
            let Some(exe) = driver.executable() else {
                continue;
            };
            // Evaluated under the lock already held: never lock the hub again from here.
            if self.drivers.get(&driver.id).is_some() && !hub.sidecars.contains_key(&driver.id) {
                tracing::warn!(driver = %driver.id, "an installed driver shadows a bundled one; ignored");
                continue;
            }
            let same = hub
                .sidecars
                .get(&driver.id)
                .is_some_and(|s| s.spec().version == driver.active && s.spec().program == exe);
            if same {
                continue;
            }
            let mut spec = SidecarSpec::new(driver.id.clone(), driver.active.clone(), exe);
            spec.args = driver.args();
            spec.idle_timeout = idle;
            let sidecar = Arc::new(SidecarDriver::with_manifest(spec, driver.manifest.clone()));
            if let Some(old) = hub.sidecars.insert(driver.id.clone(), sidecar.clone()) {
                old.restart();
            }
            self.drivers.register_shared(sidecar);
        }
        hub.installed = installed.to_vec();
    }
}

fn progress_to(shared: &Arc<Shared>, id: &str) -> impl Fn(u64, Option<u64>) + Send + Sync + use<> {
    let shared = shared.clone();
    let id = id.to_string();
    move |done, total| {
        shared
            .drivers_hub
            .inner
            .lock()
            .jobs
            .insert(id.clone(), DriverJobState::Running { done, total });
        shared.repaint();
    }
}

impl Engine {
    /// Points the engine at the install directory and the registry, and registers the
    /// drivers installed there (on the runtime: the UI thread never reads the disk).
    /// `index_url` empty means the default registry; `idle_minutes` is how long a sidecar
    /// outlives its last connection.
    pub fn configure_drivers(&self, data_dir: &Path, index_url: &str, idle_minutes: u32) {
        self.configure_drivers_with_key(data_dir, index_url, idle_minutes, REGISTRY_PUBLIC_KEY);
    }

    /// [`Engine::configure_drivers`] with another registry key (tests sign their own index).
    pub fn configure_drivers_with_key(
        &self,
        data_dir: &Path,
        index_url: &str,
        idle_minutes: u32,
        public_key: &str,
    ) {
        let url = if index_url.trim().is_empty() {
            DEFAULT_INDEX_URL
        } else {
            index_url.trim()
        };
        let registry = Arc::new(Registry::new(
            data_dir.to_path_buf(),
            url.to_string(),
            public_key,
        ));
        {
            let mut hub = self.shared.drivers_hub.inner.lock();
            hub.registry = Some(registry.clone());
            hub.idle = Duration::from_secs(u64::from(idle_minutes.max(1)) * 60);
        }
        self.reload_installed(registry);
    }

    /// Reads the install directory again and registers what it holds.
    fn reload_installed(&self, registry: Arc<Registry>) {
        let shared = self.shared.clone();
        self.shared.send(Command::Spawn(Box::pin(async move {
            let installed = tokio::task::spawn_blocking(move || registry.installed())
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r.map_err(|e| e.to_string()));
            match installed {
                Ok(installed) => {
                    shared.drivers_hub.sync_drivers(&installed);
                }
                Err(e) => tracing::warn!(error = %e, "reading installed drivers failed"),
            }
            shared.drivers_hub.loaded.store(true, Ordering::Release);
            shared.drivers_hub.bump(&shared);
        })));
    }

    /// Whether the install directory was read, so a missing driver is really missing.
    pub fn drivers_loaded(&self) -> bool {
        self.shared.drivers_hub.loaded.load(Ordering::Acquire)
    }

    /// Changes whenever the installed drivers, the index, a job or a manual install did.
    pub fn drivers_revision(&self) -> u64 {
        self.shared.drivers_hub.revision.load(Ordering::Acquire)
    }

    pub fn driver_index(&self) -> IndexState {
        self.shared.drivers_hub.inner.lock().index.clone()
    }

    pub fn installed_drivers(&self) -> Vec<InstalledDriver> {
        self.shared.drivers_hub.inner.lock().installed.clone()
    }

    /// Install, update, remove and activate jobs by driver id (the last state of each).
    pub fn driver_jobs(&self) -> Vec<(String, DriverJobState)> {
        let hub = self.shared.drivers_hub.inner.lock();
        hub.jobs
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn manual_install(&self) -> ManualState {
        self.shared.drivers_hub.inner.lock().manual.clone()
    }

    /// Fetches the registry index (the Driver Manager's Refresh).
    pub fn refresh_driver_index(&self) {
        let Some(registry) = self.shared.drivers_hub.registry() else {
            return;
        };
        self.shared.drivers_hub.inner.lock().index = IndexState::Loading;
        let shared = self.shared.clone();
        self.shared.send(Command::Spawn(Box::pin(async move {
            let state = match registry.fetch_index().await {
                Ok(entries) => IndexState::Ready(Arc::new(entries)),
                Err(e) => IndexState::Failed(e.to_string()),
            };
            shared.drivers_hub.inner.lock().index = state;
            shared.drivers_hub.bump(&shared);
        })));
        self.shared.repaint();
    }

    /// Installs (or updates to) the version the index lists for `id`.
    pub fn install_driver(&self, id: &str) {
        let hub = &self.shared.drivers_hub;
        let (Some(registry), IndexState::Ready(index)) = (hub.registry(), self.driver_index())
        else {
            self.fail_job(id, "the registry index is not loaded");
            return;
        };
        let Some(entry) = index.iter().find(|e| e.id == id).cloned() else {
            self.fail_job(id, "the registry does not list this driver");
            return;
        };
        hub.inner.lock().jobs.insert(
            id.to_string(),
            DriverJobState::Running {
                done: 0,
                total: None,
            },
        );
        let (shared, id) = (self.shared.clone(), id.to_string());
        self.shared.send(Command::Spawn(Box::pin(async move {
            let result = registry.install(&entry, progress_to(&shared, &id)).await;
            let state = match result {
                Ok(_) => DriverJobState::Done,
                Err(e) => DriverJobState::Failed(e.to_string()),
            };
            finish_job(&shared, &registry, &id, state).await;
        })));
        self.shared.repaint();
    }

    /// Makes `version` of `id` the active one (rollback).
    pub fn activate_driver(&self, id: &str, version: &str) {
        let Some(registry) = self.shared.drivers_hub.registry() else {
            return;
        };
        let (shared, id, version) = (self.shared.clone(), id.to_string(), version.to_string());
        self.shared.send(Command::Spawn(Box::pin(async move {
            let (r, i, v) = (registry.clone(), id.clone(), version);
            let result = tokio::task::spawn_blocking(move || r.activate(&i, &v)).await;
            let state = match result {
                Ok(Ok(())) => DriverJobState::Done,
                Ok(Err(e)) => DriverJobState::Failed(e.to_string()),
                Err(e) => DriverJobState::Failed(e.to_string()),
            };
            finish_job(&shared, &registry, &id, state).await;
        })));
    }

    /// Removes one version of `id`, or all of them with `None`.
    pub fn remove_driver(&self, id: &str, version: Option<&str>) {
        let Some(registry) = self.shared.drivers_hub.registry() else {
            return;
        };
        let (shared, id, version) = (
            self.shared.clone(),
            id.to_string(),
            version.map(str::to_string),
        );
        self.shared.send(Command::Spawn(Box::pin(async move {
            let (r, i) = (registry.clone(), id.clone());
            let result =
                tokio::task::spawn_blocking(move || r.remove(&i, version.as_deref())).await;
            let state = match result {
                Ok(Ok(())) => DriverJobState::Done,
                Ok(Err(e)) => DriverJobState::Failed(e.to_string()),
                Err(e) => DriverJobState::Failed(e.to_string()),
            };
            finish_job(&shared, &registry, &id, state).await;
        })));
    }

    /// Reads a driver archive the user picked. Nothing is installed or run: the result is
    /// [`ManualState::Review`], waiting for [`Engine::confirm_manual_install`].
    pub fn inspect_manual_install(&self, file: PathBuf) {
        let Some(registry) = self.shared.drivers_hub.registry() else {
            return;
        };
        self.shared.drivers_hub.inner.lock().manual = ManualState::Inspecting;
        let shared = self.shared.clone();
        self.shared.send(Command::Spawn(Box::pin(async move {
            let state = match registry.install_manual(&file).await {
                Ok(inspection) => ManualState::Review(inspection),
                Err(e) => ManualState::Failed(e.to_string()),
            };
            shared.drivers_hub.inner.lock().manual = state;
            shared.drivers_hub.bump(&shared);
        })));
        self.shared.repaint();
    }

    /// Installs the inspected file ("Install anyway").
    pub fn confirm_manual_install(&self) {
        let hub = &self.shared.drivers_hub;
        let (Some(registry), ManualState::Review(inspection)) =
            (hub.registry(), self.manual_install())
        else {
            return;
        };
        let key = inspection.id.clone();
        hub.inner.lock().jobs.insert(
            key.clone(),
            DriverJobState::Running {
                done: 0,
                total: None,
            },
        );
        hub.inner.lock().manual = ManualState::None;
        let shared = self.shared.clone();
        self.shared.send(Command::Spawn(Box::pin(async move {
            let state = match registry.confirm_manual(&inspection).await {
                Ok(_) => DriverJobState::Done,
                Err(e) => DriverJobState::Failed(e.to_string()),
            };
            finish_job(&shared, &registry, &key, state).await;
        })));
        self.shared.repaint();
    }

    pub fn cancel_manual_install(&self) {
        self.shared.drivers_hub.inner.lock().manual = ManualState::None;
        self.shared.repaint();
    }

    /// "Restart driver": kills the sidecar process of `id`; its connections fail and the
    /// next connect starts a fresh process.
    pub fn restart_driver(&self, id: &str) {
        if let Some(sidecar) = self.shared.drivers_hub.inner.lock().sidecars.get(id) {
            sidecar.restart();
        }
        self.shared.repaint();
    }

    /// Clears a finished job's state (the user dismissed it).
    pub fn clear_driver_job(&self, id: &str) {
        let mut hub = self.shared.drivers_hub.inner.lock();
        if !matches!(hub.jobs.get(id), Some(DriverJobState::Running { .. })) {
            hub.jobs.remove(id);
        }
        drop(hub);
        self.shared.drivers_hub.bump(&self.shared);
    }

    fn fail_job(&self, id: &str, message: &str) {
        self.shared
            .drivers_hub
            .inner
            .lock()
            .jobs
            .insert(id.to_string(), DriverJobState::Failed(message.to_string()));
        self.shared.drivers_hub.bump(&self.shared);
    }
}

/// Records a job's end, re-reads the install directory and registers the drivers it holds.
async fn finish_job(
    shared: &Arc<Shared>,
    registry: &Arc<Registry>,
    id: &str,
    state: DriverJobState,
) {
    let r = registry.clone();
    if let Ok(Ok(installed)) = tokio::task::spawn_blocking(move || r.installed()).await {
        shared.drivers_hub.sync_drivers(&installed);
    }
    shared
        .drivers_hub
        .inner
        .lock()
        .jobs
        .insert(id.to_string(), state);
    shared.drivers_hub.bump(shared);
}
