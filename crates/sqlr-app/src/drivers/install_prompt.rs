//! "Install" wherever a driver is missing: the new-connection catalog, a saved connection
//! from another machine, the tree's inline row.

use sqlr_engine::{DriverJobState, IndexState};
use sqlr_ui::ToastKind;

use super::PendingInstall;
use crate::app::SqlrApp;

impl SqlrApp {
    /// Installs `driver` from the registry (loading the index first if needed). With
    /// `known` set, the new-connection form for that database opens once it is installed.
    pub(crate) fn request_driver_install(&mut self, driver: &str, known: Option<&'static str>) {
        if self.drivers_ui.pending.is_some() {
            self.notify_as(ToastKind::Info, "Another driver is installing".to_string());
            return;
        }
        if matches!(
            self.engine.driver_index(),
            IndexState::Idle | IndexState::Failed(_)
        ) {
            self.engine.refresh_driver_index();
        }
        self.drivers_ui.pending = Some(PendingInstall {
            driver: driver.to_string(),
            known,
            started: false,
        });
        self.notify_as(ToastKind::Info, format!("Installing the {driver} driver…"));
    }

    /// A sidecar driver's dialect is known only after its first connection: give it to the
    /// saved connections that were waiting for it (sort and filter, quoting, highlighting).
    fn adopt_new_dialects(&mut self) {
        let mut added = false;
        for c in &self.connections {
            if self.dialects.contains_key(&c.id) {
                continue;
            }
            if let Some(dialect) = self
                .engine
                .drivers()
                .get(&c.driver)
                .and_then(|d| d.dialect())
            {
                self.dialects.insert(c.id.clone(), dialect);
                added = true;
            }
        }
        if added {
            // Tabs follow the new syntax when the listing is applied again.
            self.reload_connections();
        }
    }

    /// Advances a pending install, and refreshes what depends on the installed drivers.
    pub(crate) fn drivers_tick(&mut self) {
        let revision = self.engine.drivers_revision();
        if revision != self.drivers_ui.seen_revision {
            self.drivers_ui.seen_revision = revision;
            // The dialects and syntaxes of saved connections follow their drivers.
            self.reload_connections();
        }
        self.adopt_new_dialects();
        let Some(pending) = &self.drivers_ui.pending else {
            return;
        };
        let (driver, known, started) = (pending.driver.clone(), pending.known, pending.started);
        let job = self
            .engine
            .driver_jobs()
            .into_iter()
            .find(|(id, _)| *id == driver)
            .map(|(_, state)| state);
        if !started {
            match self.engine.driver_index() {
                IndexState::Ready(_) => {
                    self.engine.install_driver(&driver);
                    if let Some(p) = &mut self.drivers_ui.pending {
                        p.started = true;
                    }
                }
                IndexState::Failed(e) => {
                    self.drivers_ui.pending = None;
                    self.notify_as(
                        ToastKind::Error,
                        format!(
                            "Cannot reach the registry: {e}. No driver registry is published yet: \
                             install the driver from a file (Settings → Drivers… → Install from file…)."
                        ),
                    );
                }
                IndexState::Idle | IndexState::Loading => {}
            }
            return;
        }
        match job {
            Some(DriverJobState::Done) => {
                self.drivers_ui.pending = None;
                self.engine.clear_driver_job(&driver);
                self.notify_as(ToastKind::Success, format!("Installed the {driver} driver"));
                if let Some(known) = known {
                    self.open_known_connection(known);
                }
            }
            Some(DriverJobState::Failed(message)) => {
                self.drivers_ui.pending = None;
                self.notify_as(
                    ToastKind::Error,
                    format!("Could not install the {driver} driver: {message}"),
                );
            }
            Some(DriverJobState::Running { .. }) | None => {}
        }
    }
}
