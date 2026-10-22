//! The Driver Manager window.

use std::collections::HashSet;

use egui::{Id, RichText};
use sqlr_engine::{DriverJobState, IndexState, ManualState, is_newer};
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{
    DriverJobView, DriverManagerAction, DriverManagerState, DriverRow, IndexStatus, ToastKind,
    driver_manager,
};

use super::ManualPick;
use crate::app::SqlrApp;
use crate::dialogs::{Confirm, confirm_delete};

impl SqlrApp {
    /// Opens the Driver Manager; the registry is contacted now, not before.
    pub(crate) fn open_driver_manager(&mut self) {
        self.drivers_ui
            .manager
            .get_or_insert_with(DriverManagerState::default);
        if matches!(self.engine.driver_index(), IndexState::Idle) {
            self.engine.refresh_driver_index();
        }
    }

    /// The list: installed drivers, the registry's, and the ones that ship with sqlr.
    fn driver_rows(&self) -> Vec<DriverRow> {
        let installed = self.engine.installed_drivers();
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
        let index = match self.engine.driver_index() {
            IndexState::Ready(entries) => entries,
            _ => Default::default(),
        };
        let mut rows = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for driver in &installed {
            let entry = index.iter().find(|e| e.id == driver.id);
            seen.insert(driver.id.clone());
            rows.push(DriverRow {
                id: driver.id.clone(),
                name: driver.manifest.name.clone(),
                family: format!("{:?}", driver.manifest.family),
                builtin: false,
                installed: driver.versions.clone(),
                active: Some(driver.active.clone()),
                manual: driver.manual,
                available: entry.map(|e| e.version.clone()),
                update_available: entry.is_some_and(|e| is_newer(&e.version, &driver.active)),
                description: entry.map(|e| e.description.clone()).unwrap_or_default(),
                homepage: entry.map(|e| e.homepage.clone()).unwrap_or_default(),
                license: entry.map(|e| e.license.clone()).unwrap_or_default(),
                job: job_of(&driver.id),
            });
        }
        let listed = seen.clone();
        for entry in index.iter().filter(|e| !listed.contains(&e.id)) {
            seen.insert(entry.id.clone());
            rows.push(DriverRow {
                id: entry.id.clone(),
                name: entry.name.clone(),
                family: entry.family.clone(),
                builtin: false,
                installed: Vec::new(),
                active: None,
                manual: false,
                // Only a driver with a build for this platform can be installed.
                available: entry.artifact.as_ref().map(|_| entry.version.clone()),
                update_available: false,
                description: entry.description.clone(),
                homepage: entry.homepage.clone(),
                license: entry.license.clone(),
                job: job_of(&entry.id),
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
                available: None,
                update_available: false,
                description: String::new(),
                homepage: String::new(),
                license: String::new(),
                job: None,
            });
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
            if let Some(path) = chosen {
                self.engine.inspect_manual_install(path);
            }
        }
    }

    fn manager_window(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.drivers_ui.manager.take() else {
            return;
        };
        let rows = self.driver_rows();
        let status = match self.engine.driver_index() {
            IndexState::Idle => IndexStatus::Idle,
            IndexState::Loading => IndexStatus::Loading,
            IndexState::Ready(_) => IndexStatus::Ready,
            IndexState::Failed(e) => IndexStatus::Failed(e),
        };
        let mut action = None;
        let mut close = false;
        let response = egui::Modal::new(Id::new("driver_manager")).show(ctx, |ui| {
            ui.set_width(780.0);
            ui.label(RichText::new("Drivers").font(theme::semibold(ui.ctx(), 15.0)));
            ui.add_space(Spacing::S);
            action = driver_manager(ui, &mut state, &rows, &status);
            ui.add_space(Spacing::M);
            if ui.button("Close").clicked() {
                close = true;
            }
        });
        if response.should_close() {
            close = true;
        }
        if !close {
            self.drivers_ui.manager = Some(state);
        }
        match action {
            None => {}
            Some(DriverManagerAction::Refresh) => self.engine.refresh_driver_index(),
            Some(DriverManagerAction::Install(id) | DriverManagerAction::Update(id)) => {
                self.engine.install_driver(&id);
            }
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
            Some(DriverManagerAction::InstallFromFile) => {
                if self.drivers_ui.pick.is_none() {
                    self.drivers_ui.pick = Some(ManualPick::open());
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
                self.notify_as(
                    ToastKind::Error,
                    format!("Cannot install the driver: {message}"),
                );
            }
            ManualState::Review(inspection) => {
                let mut decision = None;
                egui::Modal::new(Id::new("manual_driver_review")).show(ctx, |ui| {
                    ui.set_width(520.0);
                    ui.label(
                        RichText::new("Install a driver from a file?")
                            .font(theme::semibold(ui.ctx(), 15.0)),
                    );
                    ui.add_space(Spacing::S);
                    let palette = theme::palette(ui.ctx());
                    ui.label(
                        RichText::new(
                            "This driver was not checked by the sqlr registry. It is a program \
                             that will run on this computer with your rights and will receive \
                             the passwords of the connections that use it. Install it only if \
                             you trust where it came from.",
                        )
                        .color(palette.warning),
                    );
                    ui.add_space(Spacing::S);
                    ui.label(format!("File: {}", inspection.file.display()));
                    ui.label(format!("Driver: {} {}", inspection.id, inspection.version));
                    ui.label(format!("Size: {} bytes", inspection.size));
                    ui.horizontal(|ui| {
                        ui.label("SHA-256:");
                        ui.monospace(&inspection.sha256);
                    });
                    ui.add_space(Spacing::M);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            decision = Some(false);
                        }
                        if ui.button("Install anyway").clicked() {
                            decision = Some(true);
                        }
                    });
                });
                match decision {
                    Some(true) => self.engine.confirm_manual_install(),
                    Some(false) => self.engine.cancel_manual_install(),
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
        let detail = Some("Connections that use it stop working until it is installed again.");
        match confirm_delete(ctx, &prompt, detail) {
            Confirm::Pending => {}
            Confirm::Yes => {
                self.engine.remove_driver(&id, None);
                self.drivers_ui.confirm_remove = None;
            }
            Confirm::No => self.drivers_ui.confirm_remove = None,
        }
    }
}
