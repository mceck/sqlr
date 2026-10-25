//! Fast connection migration: automatic discovery and selection, one explicit write.
use egui::{ComboBox, RichText, TextEdit};
use sqlr_core::ConnectionConfig;
use sqlr_engine::DriverRegistry;
use sqlr_store::client_migration::{
    ClientFormat, DetectedSource, ImportBatch, same_connection, supports_export,
};
use sqlr_ui::theme::{self, Spacing};

use crate::app::SqlrApp;
use crate::store_worker::{MigrationRequest, MigrationResult, StoreOp};
use crate::transfer_dialogs::FileAsk;

/// A reason to review an imported connection: `badge` is the short chip, `detail` its tooltip.
struct Note {
    badge: &'static str,
    detail: &'static str,
}

struct Row {
    /// The client this connection is read from; empty when exporting.
    origin: String,
    config: ConnectionConfig,
    selected: bool,
    disabled: Option<String>,
    notes: Vec<Note>,
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

/// `host · database · group`, skipping what the source did not provide.
fn connection_detail(c: &ConnectionConfig) -> String {
    [
        c.values.get("host").or_else(|| c.values.get("path")),
        c.values.get("database"),
        c.group.as_ref(),
    ]
    .into_iter()
    .flatten()
    .filter(|v| !v.is_empty())
    .map(String::as_str)
    .collect::<Vec<_>>()
    .join("  ·  ")
}

fn chip(ui: &mut egui::Ui, text: &str, fg: egui::Color32, bg: egui::Color32) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), egui::FontId::proportional(11.0), fg);
    let size = galley.size() + egui::vec2(16.0, 6.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 10, bg);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    response
}

/// A framed button with a leading icon; its accessible name is `label` alone.
fn action_button(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    let glyph = theme::icons::text(ui, icon, theme::icons::SIZE);
    let response = ui.add(egui::Button::new((glyph, label)));
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    response
}

/// One connection of the preview as a card: checkbox, driver icon tile, name with its
/// address, and chips for the driver, security settings and anything to review.
fn connection_row(ui: &mut egui::Ui, p: &theme::Palette, index: usize, r: &mut Row) {
    let disabled = r.disabled.is_some();
    let on = r.selected && !disabled;
    let c = &r.config;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 58.0), egui::Sense::click());
    let (fill, stroke) = if on {
        (p.accent_soft, p.accent)
    } else if response.hovered() && !disabled {
        (p.hover, p.border_input)
    } else {
        (p.bg_subtle, p.border)
    };
    ui.painter().rect(
        rect,
        8,
        fill,
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    let (name_color, detail_color) = if disabled {
        (p.text_disabled, p.text_disabled)
    } else {
        (p.text, p.text_muted)
    };
    let inner = rect.shrink2(egui::vec2(12.0, 0.0));
    let mut toggle = false;
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt(("migration-row", index))
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            toggle = ui
                .add_enabled(!disabled, egui::Checkbox::without_text(&mut r.selected))
                .changed();
            ui.add_space(Spacing::S);
            let (tile, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
            ui.painter().rect_filled(tile, 8, p.bg_input);
            theme::icons::paint_database_at(ui, tile.shrink(6.0), &c.driver);
            ui.add_space(Spacing::M);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add_space(9.0);
                ui.label(
                    RichText::new(&c.name)
                        .font(theme::semibold(ui.ctx(), 14.0))
                        .color(name_color),
                );
                let detail = connection_detail(c);
                let detail = if detail.is_empty() {
                    "No address in the source".to_string()
                } else {
                    detail
                };
                ui.label(RichText::new(detail).small().color(detail_color));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(reason) = &r.disabled {
                    chip(ui, reason, p.text_muted, p.bg_input);
                    return;
                }
                if !r.origin.is_empty() {
                    chip(ui, &format!("from {}", r.origin), p.accent, p.bg_input);
                }
                for note in &r.notes {
                    chip(ui, note.badge, p.warning, p.bg_input).on_hover_text(note.detail);
                }
                let ssh = c.values.get("ssh_enabled").is_some_and(|v| v == "true");
                for tag in [
                    ssh.then_some("SSH"),
                    c.values.contains_key("ssl_mode").then_some("TLS"),
                    c.profile.as_deref(),
                ]
                .into_iter()
                .flatten()
                {
                    chip(ui, tag, p.success, p.bg_input);
                }
                chip(ui, &c.driver, p.text_muted, p.bg_input);
            });
        },
    );
    if response.clicked() && !disabled && !toggle {
        r.selected = !r.selected;
    }
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
        let default_origin = if self.source != 0 {
            self.format.label().to_string()
        } else {
            String::new()
        };
        for (index, config) in batch.connections.into_iter().enumerate() {
            let duplicate = existing.iter().any(|c| same_connection(c, &config))
                || self
                    .rows
                    .iter()
                    .any(|r| same_connection(&r.config, &config));
            let mut notes = Vec::new();
            if drivers.get(&config.driver).is_none() {
                notes.push(Note {
                    badge: "Driver needed",
                    detail: "Driver installation required on connect",
                });
            }
            if config.driver != "sqlite" && config.values.get("user").is_none_or(String::is_empty) {
                notes.push(Note {
                    badge: "No username",
                    detail: "Username unavailable in source; review connection settings",
                });
            }
            let credentials = batch.credentials.get(&index).cloned().unwrap_or_default();
            let origin = batch
                .origins
                .get(index)
                .cloned()
                .unwrap_or_else(|| default_origin.clone());
            self.rows.push(Row {
                origin,
                credentials,
                config,
                selected: !duplicate,
                disabled: duplicate.then(|| "Already saved / duplicate".into()),
                notes,
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
        let icon = if self.importing {
            theme::icons::IMPORT
        } else {
            theme::icons::EXPORT
        };
        let subtitle = if self.importing {
            "Bring your saved connections into sqlr"
        } else {
            "Move connections to another client or save a portable backup"
        };
        // The connection list scrolls within the window; the body scrolls past it if needed.
        let list_height = (ctx.content_rect().height() - 400.0).max(110.0);
        let response = theme::Dialog::new("client_migration", icon, title)
            .subtitle(subtitle)
            .width(820.0)
            .show(ctx, |d| {
            d.body(|ui| {
                if let Some(done) = &self.done {
                    ui.add_space(Spacing::XL);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            theme::icons::text(ui, theme::icons::SUCCESS, 36.0).color(p.success),
                        );
                        ui.add_space(Spacing::M);
                        ui.label(RichText::new(done).font(theme::semibold(ctx, 15.0)));
                        if !self.importing {
                            ui.add_space(Spacing::M);
                            ui.label(RichText::new(self.format.export_help()).color(p.text_muted));
                        }
                    });
                    ui.add_space(Spacing::L);
                    self.warning_ui(ui);
                    return;
                }
                theme::section_title(
                    ui,
                    if self.importing { "Source" } else { "Destination" },
                );
                ui.add_space(Spacing::S);
                egui::Frame::new()
                    .fill(p.bg_subtle)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .corner_radius(egui::CornerRadius::same(Spacing::RADIUS_S))
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add_enabled_ui(!waiting, |ui| {
                            if self.importing {
                                ui.horizontal(|ui| {
                                    let before = self.source;
                                    ComboBox::from_id_salt("migration-source")
                                        .width((ui.available_width() - 300.0).max(180.0))
                                        .selected_text(&self.source_label)
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(&mut self.source, 0, "All detected sources");
                                            for (i, s) in self.sources.iter().enumerate() {
                                                ui.selectable_value(
                                                    &mut self.source,
                                                    i + 1,
                                                    format!("{} · {}", s.format.label(), s.path.display()),
                                                );
                                            }
                                        });
                                    if before != self.source {
                                        if self.source == 0 {
                                            request = Some(MigrationRequest::Discover);
                                        } else if let Some(s) = self.sources.get(self.source - 1) {
                                            self.source_label =
                                                format!("{} · {}", s.format.label(), s.path.display());
                                            request = Some(MigrationRequest::Read(s.path.clone()));
                                        }
                                    }
                                    if action_button(ui, theme::icons::OPEN, "Choose file…").clicked() {
                                        self.ask = Some(FileAsk::open(
                                            answer,
                                            "Import connections",
                                            &["json", "xml", "plist", "txt", "db", "tableplusconnection"],
                                        ));
                                    }
                                    if action_button(ui, theme::icons::PASTE, "Paste from clipboard").clicked() {
                                        self.paste_pending = true;
                                        sqlr_ui::keymap::request_paste(ctx);
                                        ctx.request_repaint();
                                    }
                                });
                                ui.add_space(Spacing::S);
                                let recognized = if self.source == usize::MAX {
 format!("  Recognized format: {}.", self.format.label())
 } else {
 String::new()
 };
                                ui.label(
                                    RichText::new(format!(
                                        "Pick a detected client, choose a file or drop one here.{recognized}"
                                    ))
                                    .small()
                                    .color(p.text_muted),
                                );
                            } else {
                                let before = self.format;
                                ui.horizontal(|ui| {
                                    ui.label("Export to");
                                    ComboBox::from_id_salt("migration-format")
                                        .selected_text(self.format.label())
                                        .show_ui(ui, |ui| {
                                            for f in ClientFormat::ALL {
                                                ui.selectable_value(&mut self.format, f, f.label());
                                            }
                                        });
                                });
                                if before != self.format {
                                    self.refresh_export();
                                    self.include_credentials = false;
                                }
                                ui.add_space(Spacing::S);
                                ui.add_enabled(
                                    self.format.supports_credentials(),
                                    egui::Checkbox::new(&mut self.include_credentials, "Include credentials"),
                                );
                                if self.include_credentials {
                                    ui.colored_label(
                                        p.warning,
                                        "Credentials will be saved in plaintext. Store the export securely.",
                                    );
                                } else if !self.format.supports_credentials() {
                                    ui.small("This format does not support exporting credentials.");
                                }
                                ui.add_space(Spacing::S);
                                ui.label(self.format.export_help());
                                if self.format != ClientFormat::Sqlr {
                                    ui.small("Portable connection settings only. Custom profiles and client preferences may need to be recreated.");
                                }
                            }
                        });
                        if self.importing && self.rows.iter().any(|r| !r.credentials.0.is_empty()) {
                            ui.add_space(Spacing::S);
                            ui.small("This backup includes credentials. Importing will save them in sqlr's credential store.");
                        } else if !self.include_credentials {
                            ui.add_space(Spacing::S);
                            ui.label(
                                RichText::new("Passwords stay in each client's credential store. Enter them when connecting in the destination client.")
                                .small()
                                .color(p.text_muted),
                            );
                        }
                    });
                ui.add_space(Spacing::L);
                if self.importing && self.rows.is_empty() {
                    if waiting {
                        ui.add_space(Spacing::XL);
                        ui.vertical_centered(|ui| {
                            ui.spinner();
                            ui.add_space(Spacing::M);
                            ui.label(RichText::new("Looking for connections…").color(p.text_muted));
                        });
                    } else {
                        theme::empty_panel(
                            ui,
                            theme::icons::IMPORT,
                            "No connections yet",
                            "Choose a client connection file. Its format is detected automatically.",
                        );
                        ui.collapsing("Where to find connection files", |ui| {
                            ui.label("DBeaver: <workspace>/<project>/.dbeaver/data-sources.json");
                            ui.label("DataGrip: <project>/.idea/dataSources.xml, or save Copy Data Source XML to a file.");
                            ui.label("TablePlus: Connections.plist, or connection URLs saved to a text file. Encrypted archives are not supported.");
                            ui.label("Beekeeper Studio: exported connection JSON files, or app.db in the beekeeper-studio application data folder.");
                            ui.label("Tabularis: Connections → Export → Plain text, or connections.json in the tabularis application data folder.");
                            ui.label("pgAdmin: Tools → Import/Export Servers → Export.");
                        });
                    }
                }
                if !self.rows.is_empty() {
                    theme::section_title(ui, "Connections");
                    ui.add_space(Spacing::S);
                    ui.add_enabled_ui(!waiting, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                TextEdit::singleline(&mut self.filter)
                                    .hint_text("Search connections…")
                                    .desired_width(240.0),
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
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(
                                    RichText::new(format!("{selected} of {} selected", self.rows.len()))
                                        .color(p.text_muted),
                                );
                            });
                        });
                        ui.add_space(Spacing::S);
                        let filter = self.filter.to_lowercase();
                        egui::ScrollArea::vertical()
                            .id_salt("migration-connections")
                            .max_height(list_height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                for (i, r) in self.rows.iter_mut().enumerate() {
                                    if !filter.is_empty()
                                        && !format!(
                                            "{} {} {}",
                                            r.config.name,
                                            connection_detail(&r.config),
                                            r.origin
                                        )
                                            .to_lowercase()
                                            .contains(&filter)
                                    {
                                        continue;
                                    }
                                    connection_row(ui, &p, i, r);
                                }
                            });
                    });
                }
                self.warning_ui(ui);
                if let Some(error) = &self.error {
                    ui.colored_label(p.error, error);
                }
            });
            d.footer(|f| {
                if self.done.is_some() {
                    if f.primary("Done", None) {
                        close = true;
                    }
                    return;
                }
                if waiting {
                    f.left().spinner();
                    f.left().label("Reading / saving connection settings…");
                }
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
                let blocked = if waiting {
                    Some("")
                } else if selected == 0 {
                    Some("Select at least one connection")
                } else {
                    None
                };
                if f.primary(&label, blocked) {
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
                if f.cancel() {
                    close = true;
                }
            });
            });
        if response.closed {
            close = true;
        }
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
                origin: String::new(),
                config: c.clone(),
                selected: true,
                disabled: None,
                notes: vec![],
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
        let mut other = h.state().dialog.rows[0].config.clone();
        other.name = "Analytics".into();
        other.driver = "redis".into();
        h.state_mut().dialog.rows.push(Row {
            origin: "DBeaver".into(),
            config: other,
            selected: false,
            disabled: None,
            notes: vec![],
            credentials: Default::default(),
        });
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
                check_updates: false,
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
