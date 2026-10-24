//! Fast connection migration: automatic discovery and selection, one explicit write.
use egui::{Align, ComboBox, Layout, RichText, TextEdit};
use sqlr_core::ConnectionConfig;
use sqlr_engine::DriverRegistry;
use sqlr_store::client_migration::{
    ClientFormat, DetectedSource, ImportBatch, same_connection, supports_export,
};
use sqlr_ui::theme::{self, Spacing};

use crate::app::SqlrApp;
use crate::store_worker::{MigrationRequest, MigrationResult, StoreOp};
use crate::transfer_dialogs::FileAsk;

struct Row {
    config: ConnectionConfig,
    selected: bool,
    disabled: Option<String>,
    note: Option<String>,
    credentials: sqlr_core::Secrets,
}

pub(crate) struct MigrationDialog {
    id: String,
    importing: bool,
    format: ClientFormat,
    rows: Vec<Row>,
    sources: Vec<DetectedSource>,
    source: usize,
    source_label: String,
    warnings: Vec<String>,
    filter: String,
    busy: bool,
    done: Option<String>,
    error: Option<String>,
    ask: Option<FileAsk>,
    paste_pending: bool,
    include_credentials: bool,
    secret_fields: std::collections::BTreeMap<String, Vec<String>>,
}

impl MigrationDialog {
    fn new(importing: bool) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            importing,
            format: ClientFormat::Sqlr,
            rows: vec![],
            sources: vec![],
            source: 0,
            source_label: "Automatic discovery".into(),
            warnings: vec![],
            filter: String::new(),
            busy: false,
            done: None,
            error: None,
            ask: None,
            paste_pending: false,
            include_credentials: false,
            secret_fields: Default::default(),
        }
    }

    fn selected_import(&self) -> Vec<(ConnectionConfig, sqlr_core::Secrets)> {
        self.rows
            .iter()
            .filter(|r| r.selected && r.disabled.is_none())
            .map(|r| (r.config.clone(), r.credentials.clone()))
            .collect()
    }

    fn selected(&self) -> Vec<ConnectionConfig> {
        self.rows
            .iter()
            .filter(|r| r.selected && r.disabled.is_none())
            .map(|r| r.config.clone())
            .collect()
    }

    fn load(
        &mut self,
        batch: ImportBatch,
        existing: &[ConnectionConfig],
        drivers: &DriverRegistry,
    ) {
        self.warnings = batch.warnings;
        self.rows.clear();
        for (index, config) in batch.connections.into_iter().enumerate() {
            let duplicate = existing.iter().any(|c| same_connection(c, &config))
                || self
                    .rows
                    .iter()
                    .any(|r| same_connection(&r.config, &config));
            let mut notes = Vec::new();
            if drivers.get(&config.driver).is_none() {
                notes.push("Driver installation required on connect");
            }
            if config.driver != "sqlite" && config.values.get("user").is_none_or(String::is_empty) {
                notes.push("Username unavailable in source; review connection settings");
            }
            let note = (!notes.is_empty()).then(|| notes.join(" · "));
            let credentials = batch.credentials.get(&index).cloned().unwrap_or_default();
            self.rows.push(Row {
                credentials,
                config,
                selected: !duplicate,
                disabled: duplicate.then(|| "Already saved / duplicate".into()),
                note,
            });
        }
    }

    fn refresh_export(&mut self) {
        for r in &mut self.rows {
            r.disabled = (!supports_export(self.format, &r.config))
                .then(|| "Not supported by this export format".into());
        }
    }

    pub(crate) fn receive(
        &mut self,
        id: &str,
        result: MigrationResult,
        existing: &[ConnectionConfig],
        drivers: &DriverRegistry,
    ) {
        if self.id != id {
            return;
        }
        self.busy = false;
        match result {
            MigrationResult::Sources { sources, batch } => {
                self.source_label = format!("All detected sources ({})", sources.len());
                self.sources = sources;
                self.source = 0;
                self.load(batch, existing, drivers);
            }
            MigrationResult::Loaded { format, batch } => {
                self.format = format;
                self.load(batch, existing, drivers);
            }
            MigrationResult::Imported(count) => {
                self.done = Some(format!(
                    "Imported {count} connections. They are ready in the explorer."
                ));
            }
            MigrationResult::Exported { path, warnings } => {
                self.warnings = warnings;
                self.done = Some(format!("Connections exported to {}", path.display()));
            }
            MigrationResult::Error(e) => self.error = Some(e),
        }
    }

    fn send(&mut self, store: &mut crate::store_worker::StoreWorker, request: MigrationRequest) {
        if matches!(
            &request,
            MigrationRequest::Read(_) | MigrationRequest::Discover | MigrationRequest::Clipboard(_)
        ) {
            self.rows.clear();
            self.warnings.clear();
        }
        self.paste_pending = false;
        self.id = uuid::Uuid::new_v4().to_string();
        self.busy = true;
        self.error = None;
        store.send(StoreOp::ClientMigration {
            id: self.id.clone(),
            request,
        });
    }

    fn show(
        &mut self,
        ctx: &egui::Context,
        answer: Option<&std::path::PathBuf>,
    ) -> (bool, Option<MigrationRequest>) {
        let mut request = None;
        if self.importing && !self.busy {
            if let Some(path) =
                ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()))
            {
                self.source_label = path.display().to_string();
                self.source = usize::MAX;
                request = Some(MigrationRequest::Read(path));
            }
            if self.paste_pending {
                let text = ctx.input_mut(|i| {
                    let index = i
                        .events
                        .iter()
                        .position(|e| matches!(e, egui::Event::Paste(_)))?;
                    match i.events.remove(index) {
                        egui::Event::Paste(text) => Some(text),
                        _ => None,
                    }
                });
                if let Some(text) = text {
                    self.paste_pending = false;
                    self.source_label = "Clipboard".into();
                    self.source = usize::MAX;
                    request = Some(MigrationRequest::Clipboard(text));
                }
            }
        }
        if let Some(ask) = &mut self.ask
            && let Some(path) = ask.poll(ctx)
        {
            self.ask = None;
            if let Some(path) = path {
                if self.importing {
                    self.source_label = path.display().to_string();
                    self.source = usize::MAX;
                    request = Some(MigrationRequest::Read(path));
                } else {
                    request = Some(MigrationRequest::Export {
                        path,
                        format: self.format,
                        connections: self.selected(),
                        secret_fields: (self.include_credentials
                            && self.format.supports_credentials())
                        .then(|| self.secret_fields.clone()),
                    });
                }
            }
        }
        let mut open = true;
        let mut close = false;
        let waiting = self.busy || self.ask.is_some() || request.is_some();
        let p = theme::palette(ctx);
        let title = if self.importing {
            "Import connections"
        } else {
            "Export connections"
        };
        theme::tool_dialog(
            ctx,
            &mut open,
            title,
            if self.importing {
                theme::icons::IMPORT
            } else {
                theme::icons::EXPORT
            },
            if self.importing {
                "Bring your saved connections into sqlr"
            } else {
                "Move connections to another client or save a portable backup"
            },
            820.0,
            |ui| {
                if let Some(done) = &self.done {
                    ui.label(RichText::new("3. Done").color(p.accent).strong());
                    ui.add_space(Spacing::M);
                    ui.label(done);
                    if !self.importing {
                        ui.add_space(Spacing::M);
                        ui.label(self.format.export_help());
                    }
                    self.warning_ui(ui);
                    ui.add_space(Spacing::M);
                    if ui.button("Done").clicked() {
                        close = true;
                    }
                    return;
                }
                ui.label(
                    RichText::new(if self.importing {
                        "1. Source  ›  2. Choose connections  ›  3. Import"
                    } else {
                        "1. Destination  ›  2. Choose connections  ›  3. Export"
                    })
                    .color(p.text_muted),
                );
                ui.add_space(Spacing::M);
                ui.add_enabled_ui(!waiting,|ui| {
                    if self.importing {
                        ui.horizontal(|ui| {
                            let before = self.source;
                            ComboBox::from_id_salt("migration-source").width(430.0)
                                .selected_text(&self.source_label).show_ui(ui,|ui| {
                                    ui.selectable_value(&mut self.source,0,"All detected sources");
                                    for (i,s) in self.sources.iter().enumerate() {
                                        ui.selectable_value(&mut self.source,i+1,format!("{} · {}",s.format.label(),s.path.display()));
                                    }
                                });
                            if before != self.source {
                                if self.source == 0 { request = Some(MigrationRequest::Discover); }
                                else if let Some(s) = self.sources.get(self.source-1) {
                                    self.source_label = format!("{} · {}",s.format.label(),s.path.display());
                                    request = Some(MigrationRequest::Read(s.path.clone()));
                                }
                            }
                            if ui.button("Choose file…").clicked() {
                                self.ask = Some(FileAsk::open(answer,"Import connections",&["json","xml","plist","txt","db","tableplusconnection"]));
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui.button("Paste from clipboard").clicked() {
                                self.paste_pending = true;
                                sqlr_ui::keymap::request_paste(ctx);
                                ctx.request_repaint();
                            }
                            ui.small("Or drop a connection file here.");
                        });
                        if self.source == usize::MAX { ui.small(format!("Recognized format: {}",self.format.label())); }
                        if self.rows.is_empty() && !self.busy {
                            ui.label("Choose a client connection file. Its format is detected automatically.");
                            ui.collapsing("Where to find connection files",|ui| {
                                ui.label("DBeaver: <workspace>/<project>/.dbeaver/data-sources.json");
                                ui.label("DataGrip: <project>/.idea/dataSources.xml, or save Copy Data Source XML to a file.");
                                ui.label("TablePlus: Connections.plist, or connection URLs saved to a text file. Encrypted archives are not supported.");
                                ui.label("Beekeeper Studio: exported connection JSON files, or app.db in the beekeeper-studio application data folder.");
                                ui.label("Tabularis: Connections → Export → Plain text, or connections.json in the tabularis application data folder.");
                                ui.label("pgAdmin: Tools → Import/Export Servers → Export.");
                            });
                        }
                    } else {
                        let before = self.format;
                        ui.horizontal(|ui| {
                            ui.label("Export to");
                            ComboBox::from_id_salt("migration-format").selected_text(self.format.label()).show_ui(ui,|ui| {
                                for f in ClientFormat::ALL { ui.selectable_value(&mut self.format,f,f.label()); }
                            });
                        });
                        if before != self.format { self.refresh_export(); self.include_credentials = false; }
                        ui.add_enabled(self.format.supports_credentials(), egui::Checkbox::new(&mut self.include_credentials, "Include credentials"));
                        if self.include_credentials {
                            ui.colored_label(p.warning, "Credentials will be saved in plaintext. Store the export securely.");
                        } else if !self.format.supports_credentials() {
                            ui.small("This format does not support exporting credentials.");
                        }
                        ui.label(self.format.export_help());
                        if self.format != ClientFormat::Sqlr { ui.small("Portable connection settings only. Custom profiles and client preferences may need to be recreated."); }
                    }
                });
                ui.add_space(Spacing::S);
                if self.importing && self.rows.iter().any(|r| !r.credentials.0.is_empty()) {
                    ui.small("This backup includes credentials. Importing will save them in sqlr's credential store.");
                } else if !self.include_credentials {
                    ui.label(RichText::new("Passwords stay in each client's credential store. Enter them when connecting in the destination client.").small().color(p.text_muted));
                }
                theme::hairline(ui);
                if waiting {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Reading / saving connection settings…");
                    });
                }
                ui.add_enabled_ui(!waiting, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut self.filter)
                                .hint_text("Filter connections…")
                                .desired_width(260.0),
                        );
                        if ui.button("Select all").clicked() {
                            for r in &mut self.rows {
                                r.selected = r.disabled.is_none();
                            }
                        }
                        if ui.button("Select none").clicked() {
                            for r in &mut self.rows {
                                r.selected = false;
                            }
                        }
                        let selected = self
                            .rows
                            .iter()
                            .filter(|r| r.selected && r.disabled.is_none())
                            .count();
                        ui.label(format!("{selected} selected / {} found", self.rows.len()));
                    });
                    let filter = self.filter.to_lowercase();
                    egui::ScrollArea::vertical()
                        .id_salt("migration-connections")
                        .max_height((ui.available_height() - 130.0).max(110.0))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, r) in self.rows.iter_mut().enumerate() {
                                let c = &r.config;
                                let detail = format!(
                                    "{}  ·  {}  ·  {}  ·  {}",
                                    c.driver,
                                    c.values
                                        .get("host")
                                        .or_else(|| c.values.get("path"))
                                        .map_or("", String::as_str),
                                    c.values.get("database").map_or("", String::as_str),
                                    c.group.as_deref().unwrap_or("")
                                );
                                if !filter.is_empty()
                                    && !format!("{} {detail}", c.name)
                                        .to_lowercase()
                                        .contains(&filter)
                                {
                                    continue;
                                }
                                ui.push_id(i, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.add_enabled(
                                            r.disabled.is_none(),
                                            egui::Checkbox::new(&mut r.selected, &c.name),
                                        );
                                        if let Some(reason) = &r.disabled {
                                            ui.label(
                                                RichText::new(reason).small().color(p.text_muted),
                                            );
                                        }
                                    });
                                    ui.indent("connection-detail", |ui| {
                                        ui.label(RichText::new(detail).small().color(p.text_muted));
                                        let settings = [
                                            c.values.contains_key("ssl_mode").then_some("TLS"),
                                            c.values
                                                .get("ssh_enabled")
                                                .is_some_and(|v| v == "true")
                                                .then_some("SSH"),
                                            c.profile.as_deref(),
                                        ]
                                        .into_iter()
                                        .flatten()
                                        .collect::<Vec<_>>()
                                        .join(" · ");
                                        if !settings.is_empty() {
                                            ui.small(settings);
                                        }
                                        if let Some(note) = &r.note {
                                            ui.small(note);
                                        }
                                    });
                                    ui.add_space(Spacing::XS);
                                });
                            }
                        });
                });
                self.warning_ui(ui);
                if let Some(error) = &self.error {
                    ui.colored_label(p.error, error);
                }
                theme::hairline(ui);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!waiting, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        close = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let selected = self
                            .rows
                            .iter()
                            .filter(|r| r.selected && r.disabled.is_none())
                            .count();
                        let label = format!(
                            "{} {selected} connection{}{}",
                            if self.importing { "Import" } else { "Export" },
                            if selected == 1 { "" } else { "s" },
                            if self.importing { "" } else { "…" }
                        );
                        let enabled = !waiting && selected > 0;
                        if ui
                            .add_enabled(enabled, theme::primary_button_state(ctx, enabled, &label))
                            .clicked()
                        {
                            if self.importing {
                                request = Some(MigrationRequest::Import(self.selected_import()));
                            } else {
                                self.ask = Some(FileAsk::save(
                                    answer,
                                    "Export connections",
                                    self.format.filename(),
                                ));
                            }
                        }
                    });
                });
            },
        );
        // A write stays visible until its reply arrives. Reading may be dismissed safely.
        if self.busy {
            open = true;
        } else if close {
            open = false;
        }
        (open, request)
    }

    fn warning_ui(&self, ui: &mut egui::Ui) {
        if !self.warnings.is_empty() {
            ui.collapsing(format!("{} migration notes", self.warnings.len()), |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("migration-notes")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for warning in &self.warnings {
                            ui.label(warning);
                        }
                    });
            });
        }
    }
}

impl SqlrApp {
    pub(crate) fn open_client_import(&mut self) {
        let mut dialog = MigrationDialog::new(true);
        dialog.send(&mut self.store, MigrationRequest::Discover);
        self.client_migration = Some(dialog);
    }
    pub(crate) fn open_client_export(&mut self) {
        let mut dialog = MigrationDialog::new(false);
        for c in &self.connections {
            if let Some(manifest) = self.manifest(&c.driver) {
                dialog.secret_fields.insert(
                    c.id.clone(),
                    manifest
                        .fields
                        .iter()
                        .filter(|f| f.secret)
                        .map(|f| f.key.clone())
                        .collect(),
                );
            }
        }
        dialog.rows = self
            .connections
            .iter()
            .map(|c| Row {
                config: c.clone(),
                selected: true,
                disabled: None,
                note: None,
                credentials: Default::default(),
            })
            .collect();
        self.client_migration = Some(dialog);
    }
    pub(crate) fn client_migration_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.client_migration else {
            return;
        };
        let (open, request) = dialog.show(ctx, self.file_answer.as_ref());
        if let Some(request) = request {
            dialog.send(&mut self.store, request);
        }
        if !open {
            self.client_migration = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store_worker::{StoreReply, StoreWorker};
    use egui_kittest::{Harness, kittest::Queryable};
    use sqlr_store::{ConnectionRepo, MemorySecretStore};
    use std::time::{Duration, Instant};

    const CLIPBOARD_XML: &str = r#"<data-sources><data-source name="Sales" uuid="external">
        <driver-ref>postgresql</driver-ref><jdbc-url>jdbc:postgresql://db.example/sales</jdbc-url>
        <user-name>alice</user-name></data-source></data-sources>"#;

    struct State {
        dialog: MigrationDialog,
        worker: StoreWorker,
        saved: Vec<ConnectionConfig>,
        open: bool,
        answer: Option<std::path::PathBuf>,
    }

    fn harness(dir: &std::path::Path, size: (f32, f32)) -> Harness<'static, State> {
        let repo = ConnectionRepo::open(dir, Box::new(MemorySecretStore::default())).unwrap();
        let state = State {
            dialog: MigrationDialog::new(true),
            worker: StoreWorker::spawn(repo, || {}),
            saved: vec![],
            open: true,
            answer: None,
        };
        Harness::builder().with_size(size).build_ui_state(
            |ui, state: &mut State| {
                let installed = egui::Id::new("migration-test-fonts");
                if !ui
                    .ctx()
                    .data(|d| d.get_temp::<bool>(installed))
                    .unwrap_or(false)
                {
                    theme::install_fonts(ui.ctx(), &[]);
                    ui.ctx().data_mut(|d| d.insert_temp(installed, true));
                }
                let palette = theme::palette_for(ui.ctx(), sqlr_core::ThemePref::Dark);
                theme::apply(ui.ctx(), &palette, 13.0);
                while let Some(reply) = state.worker.try_recv() {
                    if let StoreReply::ClientMigration {
                        id,
                        result,
                        listing,
                    } = reply
                    {
                        if let Some(listing) = listing {
                            state.saved = listing.connections.unwrap();
                        }
                        state
                            .dialog
                            .receive(&id, result, &state.saved, &DriverRegistry::default());
                    }
                }
                if state.open {
                    let (open, request) = state.dialog.show(ui.ctx(), state.answer.as_ref());
                    state.open = open;
                    if let Some(request) = request {
                        state.dialog.send(&mut state.worker, request);
                    }
                }
            },
            state,
        )
    }

    fn wait(h: &mut Harness<'_, State>, done: impl Fn(&State) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(h.state()) {
            assert!(Instant::now() < deadline, "migration worker did not finish");
            h.step();
            std::thread::sleep(Duration::from_millis(5));
        }
        h.run_steps(3);
    }

    #[test]
    fn export_credentials_are_opt_in_and_unavailable_for_unsupported_formats() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(dir.path(), (1100.0, 760.0));
        h.state_mut().dialog.importing = false;
        h.run_steps(3);
        assert!(!h.state().dialog.include_credentials);
        h.get_by_label("Include credentials").click();
        h.run_steps(2);
        assert!(h.state().dialog.include_credentials);
        h.get_by_label("Credentials will be saved in plaintext. Store the export securely.");
        h.state_mut().dialog.format = ClientFormat::PgAdmin;
        h.state_mut().dialog.include_credentials = false;
        h.run_steps(2);
        h.get_by_label("Include credentials").click();
        h.run_steps(2);
        assert!(!h.state().dialog.include_credentials);
        assert!(!MigrationDialog::new(false).include_credentials);
    }

    #[test]
    fn clipboard_to_selected_connection_to_saved_connection_in_one_action() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(dir.path(), (1100.0, 760.0));
        h.run_steps(3);
        h.get_by_label("Paste from clipboard").click();
        h.step();
        h.event(egui::Event::Paste(CLIPBOARD_XML.into()));
        wait(&mut h, |s| !s.dialog.rows.is_empty() && !s.dialog.busy);
        assert_eq!(h.state().dialog.selected().len(), 1);
        assert!(h.state().saved.is_empty(), "preview must not save");
        h.get_by_label("Import 1 connection").click();
        wait(&mut h, |s| s.dialog.done.is_some());
        assert_eq!(h.state().saved.len(), 1);
        assert_ne!(h.state().saved[0].id, "external");
        assert_eq!(
            h.state().saved[0].agent_access,
            sqlr_core::AgentAccess::None
        );
        h.get_by_label("Done").click();
        h.run_steps(3);
        assert!(!h.state().open, "Done closes the dialog");
    }

    #[test]
    fn duplicates_are_unselected_and_cancel_saves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(dir.path(), (1100.0, 760.0));
        let batch =
            sqlr_store::client_migration::decode(ClientFormat::Datagrip, CLIPBOARD_XML.as_bytes())
                .unwrap();
        let existing = batch.connections.clone();
        h.state_mut()
            .dialog
            .load(batch, &existing, &DriverRegistry::default());
        assert!(h.state().dialog.selected().is_empty());
        h.run_steps(3);
        h.get_by_label("Select all").click();
        h.run_steps(2);
        assert!(h.state().dialog.selected().is_empty());
        h.get_by_label("Cancel").click();
        h.run_steps(2);
        assert!(!h.state().open);
        assert!(h.state().saved.is_empty());
    }

    #[test]
    fn stale_read_replies_cannot_replace_the_current_preview() {
        let mut d = MigrationDialog::new(true);
        d.busy = true;
        d.receive(
            "old request",
            MigrationResult::Error("old error".into()),
            &[],
            &DriverRegistry::default(),
        );
        assert!(d.busy);
        assert!(d.error.is_none());
    }

    #[test]
    fn import_actions_fit_small_displays() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(dir.path(), (800.0, 600.0));
        let batch =
            sqlr_store::client_migration::decode(ClientFormat::Datagrip, CLIPBOARD_XML.as_bytes())
                .unwrap();
        h.state_mut()
            .dialog
            .load(batch, &[], &DriverRegistry::default());
        h.run_steps(6);
        let button = h.get_by_label("Import 1 connection").rect();
        assert!(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))
                .contains_rect(button),
            "action outside display: {button:?}"
        );
    }

    #[test]
    #[ignore = "renders a migration preview; requires a GPU adapter"]
    fn migration_screenshot() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(dir.path(), (1100.0, 760.0));
        let batch =
            sqlr_store::client_migration::decode(ClientFormat::Datagrip, CLIPBOARD_XML.as_bytes())
                .unwrap();
        h.state_mut()
            .dialog
            .load(batch, &[], &DriverRegistry::default());
        h.run_steps(8);
        h.render()
            .unwrap()
            .save("/tmp/sqlr-client-import.png")
            .unwrap();
    }

    #[test]
    fn export_saves_the_selected_format_and_shows_destination_instructions() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = harness(dir.path(), (1100.0, 760.0));
        let batch =
            sqlr_store::client_migration::decode(ClientFormat::Datagrip, CLIPBOARD_XML.as_bytes())
                .unwrap();
        let s = h.state_mut();
        s.dialog.importing = false;
        s.dialog.format = ClientFormat::Dbeaver;
        s.dialog.load(batch, &[], &DriverRegistry::default());
        let path = dir.path().join("export.json");
        s.answer = Some(path.clone());
        h.run_steps(3);
        h.get_by_label("Export 1 connection…").click();
        wait(&mut h, |s| s.dialog.done.is_some());
        let (format, batch) = sqlr_store::client_migration::read_source(&path).unwrap();
        assert_eq!(format, ClientFormat::Dbeaver);
        assert_eq!(batch.connections[0].name, "Sales");
        h.get_by_label(ClientFormat::Dbeaver.export_help());
        assert!(h.state().saved.is_empty());
    }

    #[test]
    fn file_menu_opens_connection_export_and_cancel_closes_it() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let repo =
            ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
        let engine = sqlr_engine::Engine::new(DriverRegistry::default(), Arc::new(|| {}));
        let app = SqlrApp::new(
            crate::AppDeps {
                engine,
                repo,
                settings: sqlr_store::Settings::default(),
                config_dir: dir.path().into(),
                data_dir: None,
                startup_notice: None,
                storage_warning: None,
                mcp_token_store: None,
                diagnostics: None,
                diagnostics_warning: None,
            },
            &egui::Context::default(),
        );
        let mut h = Harness::builder()
            .with_size((1100.0, 760.0))
            .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
        h.run_steps(3);
        h.get_by_label("File").click();
        h.run_steps(2);
        h.get_by_label("Import…");
        h.get_by_label("Export…").click();
        h.run_steps(3);
        assert!(
            h.state()
                .client_migration
                .as_ref()
                .is_some_and(|d| !d.importing)
        );
        h.get_by_label("Cancel").click();
        h.run_steps(3);
        assert!(h.state().client_migration.is_none());
    }
}
