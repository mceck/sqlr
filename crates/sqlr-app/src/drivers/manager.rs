//! The Driver Manager window.

use std::collections::HashSet;

use egui::RichText;
use sqlr_engine::{DriverJobState, ManualState};
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{
    DriverJobView, DriverManagerAction, DriverManagerState, DriverRow, ToastKind, driver_manager,
};

use super::ManualPick;
use crate::app::SqlrApp;
use crate::dialogs::{Confirm, DeleteAsk, confirm_delete};
use crate::updates::is_newer;

impl SqlrApp {
    /// Opens the Driver Manager. Non-bundled drivers download from the sqlr release or
    /// install from a local file.
    pub(crate) fn open_driver_manager(&mut self) {
        self.drivers_ui
            .manager
            .get_or_insert_with(DriverManagerState::default);
    }

    /// The list: the installed drivers, the ones that ship with sqlr and the ones the sqlr
    /// release offers to download.
    fn driver_rows(&self) -> Vec<DriverRow> {
        let installed = self.engine.installed_drivers();
        let release = self.engine.driver_release().version;
        // Offered only when it is newer than every installed version: never a downgrade.
        let download = |id: &str, installed: &[String]| {
            (super::downloadable(id) && installed.iter().all(|v| is_newer(&release, v)))
                .then(|| release.clone())
        };
        let jobs = self.engine.driver_jobs();
        let job_of = |id: &str| {
            jobs.iter()
                .find(|(j, _)| j == id)
                .map(|(_, state)| match state {
                    DriverJobState::Running { done, total } => DriverJobView::Running {
                        done: *done,
                        total: *total,
                    },
                    DriverJobState::Done => DriverJobView::Done,
                    DriverJobState::Failed(m) => DriverJobView::Failed(m.clone()),
                })
        };
        let mut rows = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for driver in &installed {
            seen.insert(driver.id.clone());
            rows.push(DriverRow {
                id: driver.id.clone(),
                name: driver.manifest.name.clone(),
                family: format!("{:?}", driver.manifest.family),
                builtin: false,
                installed: driver.versions.clone(),
                active: Some(driver.active.clone()),
                manual: driver.manual,
                description: String::new(),
                homepage: String::new(),
                license: String::new(),
                download: download(&driver.id, &driver.versions),
                job: job_of(&driver.id),
            });
        }
        for manifest in self.engine.manifests() {
            if seen.contains(&manifest.id) {
                continue;
            }
            rows.push(DriverRow {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                family: format!("{:?}", manifest.family),
                builtin: true,
                installed: Vec::new(),
                active: None,
                manual: false,
                description: format!(
                    "Included with sqlr {} · {} connection fields",
                    env!("CARGO_PKG_VERSION"),
                    manifest.fields.len()
                ),
                homepage: "https://mceck.github.io/sqlr/manual/README.html".into(),
                license: String::new(),
                download: None,
                job: None,
            });
        }
        for db in sqlr_core::known_databases() {
            if let sqlr_core::KnownStatus::Installable { driver } = db.status
                && !rows.iter().any(|r| r.id == driver)
            {
                rows.push(DriverRow {
                    id: driver.into(),
                    name: db.name.into(),
                    family: format!("{:?}", db.family),
                    builtin: false,
                    installed: vec![],
                    active: None,
                    manual: true,
                    description: db.notes.into(),
                    homepage: "https://github.com/mceck/sqlr/tree/main/docs/drivers".into(),
                    license: String::new(),
                    download: download(driver, &[]),
                    job: job_of(driver),
                });
            }
        }
        rows
    }

    /// Draws the Driver Manager and its dialogs, and advances installs (every frame).
    pub(crate) fn drivers_frame(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.drivers_ui.open_requested) {
            self.open_driver_manager();
        }
        self.drivers_tick();
        self.manager_window(ctx);
        self.manual_review(ctx);
        self.confirm_driver_removal(ctx);
        if let Some(pick) = &mut self.drivers_ui.pick
            && let Some(chosen) = pick.poll()
        {
            self.drivers_ui.pick = None;
            match chosen {
                Some(path) => self.engine.inspect_manual_install(path),
                None => {
                    // Dismissing the file dialog cancels the install that opened it.
                    self.drivers_ui.pending = None;
                }
            }
        }
    }

    fn manager_window(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.drivers_ui.manager.take() else {
            return;
        };
        let rows = self.driver_rows();
        let response = theme::Dialog::new("driver_manager", theme::icons::DATABASE, "Drivers")
            .subtitle("The database drivers sqlr can connect with")
            .width(800.0)
            .show(ctx, |d| {
                let action = d.body(|ui| driver_manager(ui, &mut state, &rows));
                d.footer(|f| f.cancel_as("Close"));
                action
            });
        let action = response.inner;
        if response.closed {
            // Closing the window gives up an install the user did not start.
            if !self.driver_install_busy() {
                self.drivers_ui.pending = None;
            }
        } else {
            self.drivers_ui.manager = Some(state);
        }
        match action {
            None => {}
            Some(DriverManagerAction::Activate { id, version }) => {
                self.engine.activate_driver(&id, &version);
            }
            Some(DriverManagerAction::Remove {
                id,
                version: Some(v),
            }) => {
                self.engine.remove_driver(&id, Some(&v));
            }
            Some(DriverManagerAction::Remove { id, version: None }) => {
                self.drivers_ui.confirm_remove = Some(id);
            }
            Some(DriverManagerAction::Download(id)) => self.engine.download_driver(&id),
            Some(DriverManagerAction::InstallFromFile) => {
                if self.drivers_ui.pick.is_none() {
                    self.drivers_ui.pick =
                        Some(ManualPick::open(self.drivers_ui.file_answer.clone()));
                }
            }
            Some(DriverManagerAction::ClearJob(id)) => self.engine.clear_driver_job(&id),
            Some(DriverManagerAction::Restart(id)) => {
                self.engine.restart_driver(&id);
                self.notify_as(ToastKind::Info, format!("Restarted the {id} driver"));
            }
        }
    }

    /// The warning before an install from a file: its name and checksum, and the program it
    /// would run with the user's rights. Nothing is installed or started before "Install
    /// anyway".
    fn manual_review(&mut self, ctx: &egui::Context) {
        match self.engine.manual_install() {
            ManualState::None | ManualState::Inspecting => {}
            ManualState::Failed(message) => {
                self.engine.cancel_manual_install();
                self.drivers_ui.pending = None;
                self.notify_as(
                    ToastKind::Error,
                    format!("Cannot install the driver: {message}"),
                );
            }
            ManualState::Review(inspection) => {
                let response = theme::Dialog::new(
                    "manual_driver_review",
                    theme::icons::WARNING,
                    "Install driver from file",
                )
                .subtitle("Review the file before sqlr runs it")
                .width(560.0)
                .danger()
                .show(ctx, |d| {
                    d.body(|ui| {
                        let palette = theme::palette(ui.ctx());
                        ui.label(
                            RichText::new(
                                "This driver was not checked by sqlr. It is a program that will \
                                 run on this computer with your rights and will receive the \
                                 passwords of the connections that use it. Install it only if \
                                 you trust where it came from.",
                            )
                            .color(palette.warning),
                        );
                        ui.add_space(Spacing::M);
                        ui.label(format!("File: {}", inspection.file.display()));
                        ui.label(format!("Driver: {} {}", inspection.id, inspection.version));
                        ui.label(format!("Size: {} bytes", inspection.size));
                        ui.horizontal(|ui| {
                            ui.label("SHA-256:");
                            ui.monospace(&inspection.sha256);
                        });
                    });
                    d.footer(|f| {
                        let install = f.danger("Install anyway", None);
                        f.cancel();
                        install
                    })
                });
                let decision = if response.inner {
                    Some(true)
                } else if response.closed {
                    Some(false)
                } else {
                    None
                };
                match decision {
                    Some(true) => self.engine.confirm_manual_install(),
                    Some(false) => {
                        self.drivers_ui.pending = None;
                        self.engine.cancel_manual_install();
                    }
                    None => {}
                }
            }
        }
    }

    fn confirm_driver_removal(&mut self, ctx: &egui::Context) {
        let Some(id) = self.drivers_ui.confirm_remove.clone() else {
            return;
        };
        let prompt = format!("Remove the driver \"{id}\" and all its versions?");
        let ask = DeleteAsk {
            title: "Remove driver",
            action: "Remove",
            prompt: &prompt,
            detail: Some("Connections that use it stop working until it is installed again."),
        };
        match confirm_delete(ctx, ask) {
            Confirm::Pending => {}
            Confirm::Yes => {
                self.engine.remove_driver(&id, None);
                self.drivers_ui.confirm_remove = None;
            }
            Confirm::No => self.drivers_ui.confirm_remove = None,
        }
    }
}
