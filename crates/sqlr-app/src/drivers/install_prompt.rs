//! "Install from file…" wherever a driver is missing: the new-connection catalog, a saved
//! connection from another machine, the tree's inline row.

use sqlr_engine::DriverJobState;
use sqlr_ui::ToastKind;

use super::PendingInstall;
use crate::app::SqlrApp;

impl SqlrApp {
    /// Asks for the local driver file. With `known` set, the new-connection form for that
    /// database opens once the driver is installed.
    pub(crate) fn request_driver_install(&mut self, driver: &str, known: Option<&'static str>) {
        if self.drivers_ui.pending.is_some() {
            self.notify_as(ToastKind::Info, "Another driver is installing".to_string());
            return;
        }
        self.drivers_ui.pending = Some(PendingInstall {
            driver: driver.to_string(),
            known,
        });
        self.open_driver_manager();
        self.notify_as(
            ToastKind::Info,
            format!("Install the {driver} driver from a file: Drivers… → Install from file…"),
        );
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
        let (driver, known) = (pending.driver.clone(), pending.known);
        let jobs = self.engine.driver_jobs();
        let job = jobs
            .iter()
            .find(|(id, _)| *id == driver)
            .map(|(_, state)| state);
        if self.engine.drivers().get(&driver).is_some() {
            self.drivers_ui.pending = None;
            self.engine.clear_driver_job(&driver);
            self.notify_as(ToastKind::Success, format!("Installed the {driver} driver"));
            if let Some(known) = known {
                self.open_known_connection(known);
            }
            return;
        }
        if let Some(DriverJobState::Failed(message)) = job {
            self.drivers_ui.pending = None;
            self.notify_as(
                ToastKind::Error,
                format!("Could not install the {driver} driver: {message}"),
            );
        }
    }
}
