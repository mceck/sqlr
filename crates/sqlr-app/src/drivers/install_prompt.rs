//! The install wherever a driver is missing: the new-connection catalog, a saved connection
//! from another machine, the tree's inline row.

use sqlr_engine::{DriverJobState, ManualState};
use sqlr_ui::ToastKind;

use super::PendingInstall;
use crate::app::SqlrApp;

impl SqlrApp {
    /// Opens the Drivers window on `driver`: on its download when the sqlr release has it,
    /// otherwise on the file picker for a local driver file. With `known` set, the
    /// new-connection form for that database opens once the driver is installed. A request
    /// left behind (its picker dismissed, its window closed) never blocks a new one; only an
    /// install that is really under way does.
    pub(crate) fn request_driver_install(&mut self, driver: &str, known: Option<&'static str>) {
        if self.driver_install_busy() {
            self.open_driver_manager();
            self.notify_as(
                ToastKind::Info,
                "Wait for the driver being installed to finish, then try again".to_string(),
            );
            return;
        }
        // A failure left from an earlier attempt must not end this request at once.
        self.engine.clear_driver_job(driver);
        self.drivers_ui.pending = Some(PendingInstall {
            driver: driver.to_string(),
            known,
        });
        self.open_driver_manager();
        if let Some(manager) = &mut self.drivers_ui.manager {
            manager.selected = Some(driver.to_string());
        }
        if !super::downloadable(driver) && self.drivers_ui.pick.is_none() {
            self.drivers_ui.pick =
                Some(super::ManualPick::open(self.drivers_ui.file_answer.clone()));
        }
    }

    /// Answers the next driver file dialog with `path` (`None`: dismissed), for tests.
    #[doc(hidden)]
    pub fn answer_driver_file(&mut self, path: Option<std::path::PathBuf>) {
        self.drivers_ui.file_answer = Some(path);
    }

    /// Whether a driver install is under way: its file picker is open, its file is being
    /// reviewed, or an install job is running.
    pub(crate) fn driver_install_busy(&self) -> bool {
        self.drivers_ui.pick.is_some()
            || !matches!(self.engine.manual_install(), ManualState::None)
            || self
                .engine
                .driver_jobs()
                .iter()
                .any(|(_, state)| matches!(state, DriverJobState::Running { .. }))
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
                .and_then(|d| d.dialect_for(c))
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
