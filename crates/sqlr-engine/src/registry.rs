//! The drivers available to the engine, by manifest id.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use sqlr_core::{Driver, DriverManifest};

/// Clones share one table: a driver registered through any clone (an installed sidecar,
/// once the Driver Manager finished) is visible through all of them.
#[derive(Default, Clone)]
pub struct DriverRegistry {
    drivers: Arc<RwLock<HashMap<String, Arc<dyn Driver>>>>,
}

impl DriverRegistry {
    /// Registers `d` under its manifest id, replacing any driver with the same id.
    pub fn register(&mut self, d: Arc<dyn Driver>) {
        self.register_shared(d);
    }

    /// [`DriverRegistry::register`] through a shared reference, for drivers that arrive
    /// while the engine runs.
    pub fn register_shared(&self, d: Arc<dyn Driver>) {
        self.drivers.write().insert(d.manifest().id.clone(), d);
    }

    pub fn unregister(&self, id: &str) {
        self.drivers.write().remove(id);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Driver>> {
        self.drivers.read().get(id).cloned()
    }

    /// Manifests of every registered driver, sorted by name.
    pub fn manifests(&self) -> Vec<DriverManifest> {
        let mut out: Vec<_> = self
            .drivers
            .read()
            .values()
            .map(|d| d.manifest().clone())
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}
