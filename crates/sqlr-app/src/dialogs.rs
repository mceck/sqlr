//! The connection dialog, the settings window and the delete confirmation, and the app's
//! connection create/edit/delete flow around them.
//!
//! Saved connections and their secrets are read and written on the store thread
//! ([`crate::store_worker`]); `Settings::save` still runs on an explicit click.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use egui::{
    Align, Align2, Color32, ComboBox, CornerRadius, FontId, Frame, Id, Layout, Margin, RichText,
    Sense, WidgetInfo, WidgetType, vec2,
};
use sqlr_core::{
    AgentAccess, ConnectionConfig, ConnectionProfile, DriverManifest, FieldType, KnownDatabase,
    KnownStatus, NodePath, Secrets,
};
use sqlr_engine::{Engine, SessionStatus};
use sqlr_ui::text_edit_secret;
use sqlr_ui::theme::{self, Spacing, icons};
use sqlr_ui::{
    FormAction, FormState, TestStatus, connection_form_body, connection_form_status,
    connection_identity,
};

use crate::app::SqlrApp;
use crate::prefs::Scopes;
use crate::store_worker::StoreOp;

/// The connection test's result line: `"<product> <version> · <ms> ms"`, and the SSH host
/// key when a tunnel was used.
pub(crate) fn test_summary(
    product: &str,
    version: &str,
    report: Option<&sqlr_engine::ConnectReport>,
) -> String {
    let mut out = format!("{product} {version}");
    if let Some(report) = report {
        out.push_str(&format!(" · {} ms", report.ms));
        if let Some(key) = &report.host_key {
            let new = if key.new { " (new, recorded)" } else { "" };
            out.push_str(&format!(" · SSH host key {}{new}", key.fingerprint));
        }
    }
    out
}

/// Asks for the secrets of a connection that does not save them; they are kept in memory
/// for the app's lifetime.
pub struct SecretPrompt {
    conn: String,
    name: String,
    /// `(key, label)` of each secret field asked for.
    fields: Vec<(String, String)>,
    values: std::collections::BTreeMap<String, String>,
    /// A run waiting for the connection.
    pub(crate) resume: Option<(u64, crate::confirm::PendingKind)>,
}

impl SecretPrompt {
    /// Asks for the manifest's secret fields that apply to `cfg` (SSH ones only with SSH on).
    pub fn new(cfg: &ConnectionConfig, manifest: &DriverManifest) -> Self {
        let ssh_on = cfg.values.get("ssh_enabled").map(String::as_str) == Some("true");
        let ssh_auth = cfg.value(manifest, "ssh_auth").unwrap_or("password");
        let fields = manifest
            .fields
            .iter()
            .filter(|f| f.secret)
            .filter(|f| match f.key.as_str() {
                "ssh_password" => ssh_on && ssh_auth == "password",
                "ssh_key_passphrase" => ssh_on && ssh_auth == "key",
                _ => f.group != "SSH",
            })
            .map(|f| (f.key.clone(), f.label.clone()))
            .collect();
        Self {
            conn: cfg.id.clone(),
            name: cfg.name.clone(),
            fields,
            values: Default::default(),
            resume: None,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum DialogEvent {
    None,
    Closed,
}

/// Creates or edits one saved connection.
pub struct ConnDialog {
    /// The id being edited; `None` for a new connection.
    editing: Option<String>,
    driver: String,
    form: FormState,
    /// The temporary `test:<uuid>` session of the last "Test", if any.
    test_id: Option<String>,
    error: Option<String>,
    /// Secret fields that held a stored value when the dialog opened. Only these may be
    /// deleted by emptying them; a failed keychain read leaves this empty.
    stored_secrets: Vec<String>,
    /// "Manage profiles…" was clicked; the app opens the profiles dialog.
    pub manage_profiles: bool,
    /// "More databases…" was clicked; the app opens the known-databases catalog.
    pub open_catalog: bool,
    /// A native file dialog for a `File` field, polled every frame.
    pick: Option<PendingPick>,
    /// The drafts of the other data sources shown in the left list, keyed by connection id
    /// (`""` for the new one). Switching back keeps unsaved edits.
    others: HashMap<String, Draft>,
    /// Requests for the store thread, sent by the app after each frame.
    outbox: Vec<StoreOp>,
    /// A save was sent and has not been answered.
    saving: bool,
}

/// The editable state of a data source that is not the active one.
struct Draft {
    driver: String,
    form: FormState,
    error: Option<String>,
    stored_secrets: Vec<String>,
}

/// The folder of the field's current file, for the dialog to open in. No filesystem check:
/// this runs on the UI thread, and the dialog itself copes with a missing folder.
fn start_dir(current: &str) -> Option<&Path> {
    Path::new(current)
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
}

/// An open file dialog and the field it fills.
struct PendingPick {
    key: String,
    file: Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>>,
}

/// One row of the data-source list: the database mark, the name and the profile color.
fn source_row(
    ui: &mut egui::Ui,
    name: &str,
    driver: &str,
    selected: bool,
    tint: Option<[u8; 3]>,
) -> egui::Response {
    let p = theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let fill = if selected {
        p.selection
    } else if response.hovered() {
        p.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
    let icon = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 16.0, rect.center().y),
        vec2(16.0, 16.0),
    );
    icons::paint_database_at(ui, icon, driver);
    ui.painter().text(
        egui::pos2(rect.left() + 32.0, rect.center().y),
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(13.0),
        p.text,
    );
    if let Some([r, g, b]) = tint {
        ui.painter().circle_filled(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            4.0,
            Color32::from_rgb(r, g, b),
        );
    }
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, name));
    response.on_hover_text(name)
}

/// One entry of a preferences/dialog navigation rail.
pub(crate) fn nav_item(ui: &mut egui::Ui, icon: &str, label: &str, active: bool) -> egui::Response {
    let p = theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let fill = if active {
        p.accent_soft
    } else if response.hovered() {
        p.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
    ui.painter().text(
        egui::pos2(rect.left() + 18.0, rect.center().y),
        Align2::CENTER_CENTER,
        icon,
        icons::font(ui, 15.0),
        if active { p.accent } else { p.text_muted },
    );
    ui.painter().text(
        egui::pos2(rect.left() + 32.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        if active { p.text } else { p.text_muted },
    );
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, active, label));
    response
}

impl PendingPick {
    /// Opens the native dialog for the `File` field `key`; `None` for any other field.
    fn open(manifest: &DriverManifest, key: &str, current: &str) -> Option<Self> {
        let field = manifest.fields.iter().find(|f| f.key == key)?;
        let FieldType::File { save, extensions } = &field.ty else {
            return None;
        };
        let mut dialog = rfd::AsyncFileDialog::new().set_title(&field.label);
        if !extensions.is_empty() {
            dialog = dialog.add_filter(&field.label, extensions);
        }
        if let Some(dir) = start_dir(current) {
            dialog = dialog.set_directory(dir);
        }
        let file: Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>> = if *save {
            Box::pin(dialog.save_file())
        } else {
            Box::pin(dialog.pick_file())
        };
        Some(Self {
            key: key.to_string(),
            file,
        })
    }

    /// Polls without blocking: `Some(path)` once a file was chosen, `Some(None)` when the
    /// dialog was dismissed, `None` while it is still open.
    fn poll(&mut self) -> Option<Option<String>> {
        let mut cx = Context::from_waker(Waker::noop());
        match self.file.as_mut().poll(&mut cx) {
            Poll::Ready(file) => Some(file.map(|f| f.path().display().to_string())),
            Poll::Pending => None,
        }
    }
}

impl ConnDialog {
    pub fn create(manifest: &DriverManifest) -> Self {
        Self::create_in(manifest, None)
    }

    /// A new connection prefilled with `folder` (the explorer's "New connection" on a
    /// folder).
    pub fn create_in(manifest: &DriverManifest, folder: Option<&str>) -> Self {
        let mut dialog = Self {
            editing: None,
            driver: manifest.id.clone(),
            form: FormState::new(manifest),
            test_id: None,
            error: None,
            stored_secrets: Vec::new(),
            manage_profiles: false,
            open_catalog: false,
            pick: None,
            others: HashMap::new(),
            outbox: Vec::new(),
            saving: false,
        };
        if let Some(folder) = folder {
            dialog.form.group = folder.to_string();
        }
        dialog
    }

    /// A new connection for a known database: its driver, and the fields it presets (a
    /// compatible database is also named after itself).
    pub fn create_known(manifest: &DriverManifest, known: &KnownDatabase) -> Self {
        let mut dialog = Self::create(manifest);
        dialog.apply_known(manifest, known);
        dialog
    }

    fn apply_known(&mut self, manifest: &DriverManifest, known: &KnownDatabase) {
        self.form.apply_preset(manifest, known.preset());
        if matches!(known.status, KnownStatus::Compatible { .. }) && self.form.name.is_empty() {
            self.form.name = known.name.to_string();
        }
    }

    /// Switches the open dialog to a new data source for a known database (picked in the
    /// catalog while the dialog was open); the current draft is kept in the list.
    pub(crate) fn start_known(
        &mut self,
        engine: &Engine,
        manifest: &DriverManifest,
        known: &KnownDatabase,
    ) {
        self.switch(engine, None, Some(manifest));
        // Already on a new data source: start it over on the known database's driver.
        let fresh = Self::create_known(manifest, known);
        self.driver = fresh.driver;
        self.form = fresh.form;
        self.error = None;
        self.stored_secrets = Vec::new();
        self.pick = None;
    }

    /// The driver, the field values and the name being edited.
    pub(crate) fn form_values(
        &self,
    ) -> (String, std::collections::BTreeMap<String, String>, String) {
        (
            self.driver.clone(),
            self.form.values.clone(),
            self.form.name.clone(),
        )
    }

    pub fn edit(cfg: &ConnectionConfig, secrets: &Secrets, manifest: &DriverManifest) -> Self {
        Self {
            editing: Some(cfg.id.clone()),
            driver: manifest.id.clone(),
            form: FormState::from_config(cfg, secrets, manifest),
            test_id: None,
            error: None,
            stored_secrets: secrets
                .0
                .iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(k, _)| k.clone())
                .collect(),
            manage_profiles: false,
            open_catalog: false,
            pick: None,
            others: HashMap::new(),
            outbox: Vec::new(),
            saving: false,
        }
    }

    /// Edits `cfg`; its stored secrets are asked of the store thread and fill the secret
    /// fields when they arrive ([`ConnDialog::secrets_arrived`]). Until then, and if the
    /// keychain cannot be read, saving keeps whatever the keychain holds.
    pub fn open_edit(cfg: &ConnectionConfig, manifest: &DriverManifest) -> Self {
        let mut dialog = Self::edit(cfg, &Secrets::default(), manifest);
        dialog.outbox.push(StoreOp::SecretsFor {
            cfg: cfg.clone(),
            manifest: manifest.clone(),
        });
        dialog
    }

    /// The requests this dialog has for the store thread.
    pub(crate) fn take_ops(&mut self) -> Vec<StoreOp> {
        std::mem::take(&mut self.outbox)
    }

    /// The stored secrets of connection `id` (the one edited, or a draft of the list) were
    /// read: they fill the secret fields the user left empty. If the keychain could not be
    /// read, the dialog says so.
    pub(crate) fn secrets_arrived(&mut self, id: &str, result: Result<Secrets, String>) {
        let (form, error, stored) = if self.editing.as_deref() == Some(id) {
            (&mut self.form, &mut self.error, &mut self.stored_secrets)
        } else if let Some(draft) = self.others.get_mut(id) {
            (&mut draft.form, &mut draft.error, &mut draft.stored_secrets)
        } else {
            return;
        };
        match result {
            Ok(secrets) => {
                for (key, value) in secrets.0 {
                    if value.is_empty() {
                        continue;
                    }
                    stored.push(key.clone());
                    let entry = form.secrets.entry(key).or_default();
                    if entry.is_empty() {
                        *entry = value;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(conn = %id, error = %e, "reading secrets failed");
                *error = Some(format!(
                    "Could not read the saved secrets ({e}). They are kept unless you enter new ones."
                ));
            }
        }
    }

    /// The store answered this dialog's save: `Err` keeps the dialog open with the error.
    pub(crate) fn save_failed(&mut self, id: &str, e: &str) {
        self.saving = false;
        tracing::warn!(conn = %id, error = %e, "saving the connection failed");
        self.error = Some(format!("Could not save: {e}"));
        // Review Focus 5: the config may be saved even though the secret was not.
        self.editing = Some(id.to_string());
    }

    /// Whether a save is on its way to the store.
    pub(crate) fn is_saving(&self) -> bool {
        self.saving
    }

    fn test_status(&self, engine: &Engine) -> Option<TestStatus> {
        let id = self.test_id.as_deref()?;
        match engine.session_status(id)? {
            SessionStatus::Connecting => Some(TestStatus::Running),
            SessionStatus::Connected(info) => {
                let report = engine.connect_report(id);
                Some(TestStatus::Ok(test_summary(
                    &info.product,
                    &info.version,
                    report.as_ref(),
                )))
            }
            SessionStatus::Failed(e) => Some(TestStatus::Failed(e.message)),
            SessionStatus::Disconnected => None,
        }
    }

    /// Ends the test session, if one was started. Call when the dialog goes away.
    pub fn finish(&mut self, engine: &Engine) {
        if let Some(id) = self.test_id.take() {
            engine.disconnect(&id);
        }
    }

    /// Keeps the current draft in `others`, then edits `cfg` (or creates a data source when
    /// it is `None`). Unsaved edits survive switching back and forth.
    fn switch(
        &mut self,
        engine: &Engine,
        target: Option<(&ConnectionConfig, &DriverManifest)>,
        fallback: Option<&DriverManifest>,
    ) {
        let key = self.editing.clone().unwrap_or_default();
        let wanted = target.map_or(String::new(), |(cfg, _)| cfg.id.clone());
        if key == wanted {
            return;
        }
        self.finish(engine);
        self.others.insert(
            key,
            Draft {
                driver: self.driver.clone(),
                form: self.form.clone(),
                error: self.error.clone(),
                stored_secrets: self.stored_secrets.clone(),
            },
        );
        if let Some(draft) = self.others.remove(&wanted) {
            self.driver = draft.driver;
            self.form = draft.form;
            self.error = draft.error;
            self.stored_secrets = draft.stored_secrets;
        } else {
            let fresh = match target {
                Some((cfg, manifest)) => {
                    // Its secrets are asked for under its own id (a draft until they come).
                    self.outbox.push(StoreOp::SecretsFor {
                        cfg: cfg.clone(),
                        manifest: (*manifest).clone(),
                    });
                    ConnDialog::edit(cfg, &Secrets::default(), manifest)
                }
                None => match fallback {
                    Some(manifest) => ConnDialog::create(manifest),
                    None => return,
                },
            };
            self.driver = fresh.driver;
            self.form = fresh.form;
            self.error = fresh.error;
            self.stored_secrets = fresh.stored_secrets;
        }
        self.editing = target.map(|(cfg, _)| cfg.id.clone());
        self.pick = None;
        self.test_id = None;
    }

    /// The saved data sources in the dialog's left list.
    fn source_list(
        &mut self,
        ui: &mut egui::Ui,
        connections: &[ConnectionConfig],
        profiles: &[ConnectionProfile],
        switch_to: &mut Option<String>,
        new_clicked: &mut bool,
    ) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Data sources").font(theme::semibold(ui.ctx(), 12.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icons::icon_button(ui, icons::ADD, None, "New data source").clicked() {
                    *new_clicked = true;
                }
                if icons::icon_button(ui, icons::DATABASE, None, "More databases…").clicked() {
                    self.open_catalog = true;
                }
            });
        });
        ui.add_space(Spacing::S);
        theme::hint(ui, "Project data sources");
        ui.add_space(Spacing::XS);
        egui::ScrollArea::vertical()
            .id_salt("conn_dialog_sources")
            .auto_shrink(false)
            .show(ui, |ui| {
                if connections.is_empty() {
                    ui.add_space(Spacing::S);
                    theme::hint(ui, "No saved data sources yet.");
                }
                if source_row(
                    ui,
                    "New connection",
                    &self.driver,
                    self.editing.is_none(),
                    None,
                )
                .clicked()
                {
                    *new_clicked = true;
                }
                for cfg in connections {
                    let selected = self.editing.as_deref() == Some(cfg.id.as_str());
                    let tint = cfg
                        .profile
                        .as_deref()
                        .and_then(|id| profiles.iter().find(|p| p.id == id))
                        .and_then(|p| p.color);
                    if source_row(ui, &cfg.name, &cfg.driver, selected, tint).clicked() {
                        *switch_to = Some(cfg.id.clone());
                    }
                }
            });
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        engine: &Engine,
        connections: &[ConnectionConfig],
        profiles: &[ConnectionProfile],
    ) -> DialogEvent {
        let manifests = engine.manifests();
        if let Some(pick) = &mut self.pick {
            match pick.poll() {
                Some(chosen) => {
                    if let Some(path) = chosen {
                        if self.form.name.trim().is_empty() && pick.key == "path" {
                            self.form.name = Path::new(&path)
                                .file_stem()
                                .map_or(String::new(), |s| s.to_string_lossy().into_owned());
                        }
                        self.form.values.insert(pick.key.clone(), path);
                    }
                    self.pick = None;
                }
                None => ctx.request_repaint_after(Duration::from_millis(100)),
            }
        }
        let p = theme::palette(ctx);
        let mut action = FormAction::None;
        let mut switch_to = None;
        let mut new_clicked = false;
        let title = if self.editing.is_some() {
            "Edit connection"
        } else {
            "New connection"
        };
        let response = theme::Dialog::new("connection_dialog", icons::DATA_SOURCE, title)
            .subtitle("Saved connections and driver settings")
            .width(960.0)
            .body_height(540.0)
            .min_body_height(240.0)
            .body_margin(Margin::ZERO)
            .show(ctx, |d| {
                d.body(|ui| {
                    // The saved data sources.
                    egui::Panel::left(Id::new("conn_dialog_list"))
                        .resizable(false)
                        .exact_size(220.0)
                        .frame(
                            Frame::new()
                                .fill(p.bg_subtle)
                                .inner_margin(Margin::same(10)),
                        )
                        .show(ui, |ui| {
                            self.source_list(
                                ui,
                                connections,
                                profiles,
                                &mut switch_to,
                                &mut new_clicked,
                            );
                        });

                    egui::CentralPanel::no_frame().show(ui, |ui| {
                        Frame::new()
                            .fill(p.bg_raised)
                            .inner_margin(Margin::symmetric(20, 14))
                            .show(ui, |ui| {
                                let Some(manifest) = manifests.iter().find(|m| m.id == self.driver)
                                else {
                                    ui.colored_label(
                                        p.error,
                                        format!("Driver \"{}\" is not available", self.driver),
                                    );
                                    return;
                                };
                                // The whole form scrolls, with a fade where fields hide, so
                                // nothing is cut off on a short window.
                                let error_room = if self.error.is_some() { 28.0 } else { 0.0 };
                                let height = (ui.available_height() - error_room).max(80.0);
                                theme::scroll_body(ui, "conn_dialog_form", height, height, |ui| {
                                    self.driver_row(ui, &manifests);
                                    ui.add_space(Spacing::M);
                                    let body = connection_form_body(ui, &mut self.form, manifest);
                                    if body != FormAction::None { action = body; }
                                    ui.add_space(Spacing::L);
                                    theme::section_title(ui, "Connection details");
                                    ui.add_space(Spacing::S);
                                    connection_identity(ui, &mut self.form);
                                    if manifest.fields.iter().any(|f| f.secret && (f.group != "SSH" || self.form.values.get("ssh_enabled").is_some_and(|v| v == "true"))) {
                                        ui.checkbox(&mut self.form.save_secrets, "Save password securely on this device")
                                            .on_hover_text("Otherwise sqlr asks for the password when it connects");
                                    }
                                    let production = self.form.name.to_lowercase().contains("prod")
                                        || self.form.values.get("host").is_some_and(|h| h.to_lowercase().contains("prod"));
                                    if production && self.form.profile.as_deref() != Some(ConnectionProfile::PRODUCTION) {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label("This looks like a production connection.");
                                            if ui.link("Mark as Production").clicked() { self.form.profile = Some(ConnectionProfile::PRODUCTION.into()); }
                                        });
                                        theme::hint(ui, "The Production profile asks before writes and uses a red connection color.");
                                    }
                                    egui::CollapsingHeader::new("Advanced").id_salt("connection_advanced").show(ui, |ui| {
                                        self.profile_row(ui, profiles);
                                        Self::agent_access_row(ui, &mut self.form.agent_access);
                                    });
                                    ui.add_space(Spacing::M);
                                });
                                if let Some(message) = &self.error {
                                    ui.colored_label(p.error, message);
                                }
                            });
                    });
                });
                let test = self.test_status(engine);
                let blocked = match manifests.iter().find(|m| m.id == self.driver) {
                    None => Some("The driver is not available".to_string()),
                    // Saving: the button waits silently for the store.
                    Some(_) if self.saving => Some(String::new()),
                    Some(m) => self.form.validate(m).into_iter().next(),
                };
                d.footer(|f| {
                    if f.secondary("Test connection", true) {
                        action = FormAction::Test;
                    }
                    connection_form_status(f.left(), test.as_ref());
                    if f.primary("Save", blocked.as_deref()) {
                        action = FormAction::Save;
                    }
                    f.cancel();
                });
            });
        if response.closed && action == FormAction::None {
            action = FormAction::Cancel;
        }

        if new_clicked {
            let preferred = manifests
                .iter()
                .find(|m| m.id == "postgres")
                .or_else(|| manifests.first());
            self.switch(engine, None, preferred);
        } else if let Some(id) = switch_to {
            let target = connections.iter().find(|c| c.id == id).and_then(|cfg| {
                manifests
                    .iter()
                    .find(|m| m.id == cfg.driver)
                    .map(|m| (cfg, m))
            });
            if let Some((cfg, manifest)) = target {
                self.switch(engine, Some((cfg, manifest)), manifests.first());
            }
        }

        let manifest = manifests.iter().find(|m| m.id == self.driver);
        match (action, manifest) {
            (FormAction::Test, Some(m)) => {
                self.test(engine, m);
                DialogEvent::None
            }
            (FormAction::Save, Some(m)) => self.save(m),
            (FormAction::PickFile { key }, Some(m)) => {
                if self.pick.is_none() {
                    let current = self.form.values.get(&key).cloned().unwrap_or_default();
                    self.pick = PendingPick::open(m, &key, &current);
                }
                DialogEvent::None
            }
            (FormAction::Cancel, _) => {
                self.finish(engine);
                DialogEvent::Closed
            }
            _ => DialogEvent::None,
        }
    }

    /// The driver picker, with the database mark of the selected driver.
    fn driver_row(&mut self, ui: &mut egui::Ui, manifests: &[DriverManifest]) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::M;
            ui.allocate_ui_with_layout(
                vec2(96.0, ui.spacing().interact_size.y),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_width(96.0);
                    ui.label("Driver");
                },
            );
            let p = theme::palette(ui.ctx());
            let current = manifests.iter().find(|m| m.id == self.driver);
            let name = current.map_or("Unknown driver", |m| m.name.as_str());
            let mut job = egui::text::LayoutJob::default();
            job.append(
                icons::DATABASE,
                0.0,
                egui::text::TextFormat::simple(theme::fonts::icons(ui.ctx(), 14.0), p.icon_table),
            );
            job.append(
                name,
                6.0,
                egui::text::TextFormat::simple(FontId::proportional(13.0), p.text),
            );
            let width = (ui.available_width() - 8.0).max(200.0);
            ComboBox::from_id_salt("connection_driver")
                .selected_text(job)
                .width(width)
                .show_ui(ui, |ui| {
                    for m in manifests {
                        if ui.selectable_label(m.id == self.driver, &m.name).clicked()
                            && m.id != self.driver
                        {
                            self.switch_driver(m);
                        }
                    }
                });
        });
    }

    /// The connection profile picker and "Manage profiles…".
    fn profile_row(&mut self, ui: &mut egui::Ui, profiles: &[ConnectionProfile]) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::M;
            ui.allocate_ui_with_layout(
                vec2(96.0, ui.spacing().interact_size.y),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_width(96.0);
                    ui.label("Profile");
                },
            );
            let current = self
                .form
                .profile
                .clone()
                .unwrap_or_else(|| ConnectionProfile::DEVELOPMENT.to_string());
            let current = current.as_str();
            let name = profiles
                .iter()
                .find(|p| p.id == current)
                .map_or(current, |p| p.name.as_str());
            ComboBox::from_id_salt("connection_profile")
                .selected_text(name)
                .width(320.0)
                .show_ui(ui, |ui| {
                    for p in profiles {
                        if ui.selectable_label(p.id == current, &p.name).clicked() {
                            self.form.profile =
                                (p.id != ConnectionProfile::DEVELOPMENT).then(|| p.id.clone());
                        }
                    }
                });
            if ui.button("Manage profiles…").clicked() {
                self.manage_profiles = true;
            }
        });
    }

    /// The agent exposure control: human-only, and never applied by the agent write path.
    fn agent_access_row(ui: &mut egui::Ui, access: &mut AgentAccess) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::M;
            ui.allocate_ui_with_layout(
                vec2(96.0, ui.spacing().interact_size.y),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_width(96.0);
                    ui.label("Agent access");
                },
            );
            for (value, label, tip) in [
                (
                    AgentAccess::None,
                    "None",
                    "Agents cannot see this connection",
                ),
                (
                    AgentAccess::Read,
                    "Read",
                    "Agents may browse and run reads only",
                ),
                (
                    AgentAccess::Write,
                    "Write",
                    "Agents may write; the profile's confirmations still apply",
                ),
            ] {
                if ui
                    .selectable_label(*access == value, label)
                    .on_hover_text(tip)
                    .clicked()
                {
                    *access = value;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(96.0 + ui.spacing().item_spacing.x);
            theme::hint(
                ui,
                "Agents act with your rights. None is the default; Write still follows the profile's read-only and confirmation rules.",
            );
        });
    }

    /// Keeps name, group, color, profile and "Save password"; the fields start from the new
    /// driver's defaults.
    fn switch_driver(&mut self, manifest: &DriverManifest) {
        let mut form = FormState::new(manifest);
        form.name = std::mem::take(&mut self.form.name);
        form.group = std::mem::take(&mut self.form.group);
        form.color = self.form.color;
        form.profile = self.form.profile.take();
        form.save_secrets = self.form.save_secrets;
        form.agent_access = self.form.agent_access;
        self.form = form;
        self.driver = manifest.id.clone();
        self.pick = None;
    }

    fn test(&mut self, engine: &Engine, manifest: &DriverManifest) {
        self.finish(engine);
        let id = format!("test:{}", uuid::Uuid::new_v4());
        let (cfg, secrets) = self.form.to_config(id.clone(), manifest);
        engine.connect(cfg, secrets, ConnectionProfile::development());
        self.test_id = Some(id);
    }

    /// Sends the save to the store thread; the app ends it when the store answers
    /// ([`SqlrApp::connection_save_ended`]).
    fn save(&mut self, manifest: &DriverManifest) -> DialogEvent {
        let id = self
            .editing
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let (cfg, mut secrets) = self.form.to_config(id, manifest);
        // A stored secret the user emptied must be deleted, not kept. Secrets that were never
        // read (keychain error, or not read yet) are left untouched.
        for key in &self.stored_secrets {
            if manifest.fields.iter().any(|f| f.secret && &f.key == key) {
                secrets.0.entry(key.clone()).or_default();
            }
        }
        self.saving = true;
        self.outbox.push(StoreOp::Save {
            cfg,
            secrets,
            editing: self.editing.is_some(),
        });
        DialogEvent::None
    }
}

#[derive(Debug, PartialEq)]
pub enum Confirm {
    Pending,
    Yes,
    No,
}

/// What a deletion confirmation says.
pub struct DeleteAsk<'a> {
    /// Names the action and its object: "Delete connection".
    pub title: &'a str,
    /// The red button: "Delete", "Remove", "Clear history".
    pub action: &'a str,
    pub prompt: &'a str,
    /// A muted line on what else goes away.
    pub detail: Option<&'a str>,
}

/// Asks before deleting something: the shared confirmation, Cancel focused.
pub fn confirm_delete(ctx: &egui::Context, ask: DeleteAsk<'_>) -> Confirm {
    let spec = crate::confirm::ConfirmSpec::new(
        Id::new("confirm_delete"),
        icons::DELETE,
        ask.title,
        ask.action,
    )
    .width(480.0);
    let answer = crate::confirm::confirm_modal(
        ctx,
        spec,
        |ui| {
            ui.label(ask.prompt);
            if let Some(detail) = ask.detail {
                ui.add_space(Spacing::XS);
                ui.label(egui::RichText::new(detail).color(theme::palette(ui.ctx()).text_muted));
            }
        },
        |_| {},
    );
    match answer {
        None => Confirm::Pending,
        Some(true) => Confirm::Yes,
        Some(false) => Confirm::No,
    }
}

impl SqlrApp {
    pub(crate) fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(dialog) = &mut self.conn_dialog {
            let event = dialog.show(ctx, &self.engine, &self.connections, &self.profiles);
            for op in dialog.take_ops() {
                self.store.send(op);
            }
            let catalog = std::mem::take(&mut dialog.open_catalog);
            if std::mem::take(&mut dialog.manage_profiles) {
                self.open_profiles();
            }
            if catalog {
                self.databases_dialog = Some(crate::databases_dialog::DatabasesDialog::catalog());
            }
            match event {
                DialogEvent::None => {}
                DialogEvent::Closed => self.conn_dialog = None,
            }
        }
        self.profiles_ui(ctx);
        self.secret_prompt_ui(ctx);
        let agent_rows = self.agent_access_rows();
        let mut access_changes = Vec::new();
        if let Some(window) = &mut self.prefs {
            let scopes = Scopes::new(&self.profiles, &self.connections);
            window.mcp = crate::agents::McpPrefs {
                status: self.mcp.status.clone(),
                url: self.mcp.url(),
                sessions: self.mcp.sessions(),
                error: self.mcp.last_error.clone(),
                token_present: self.mcp.token().is_some(),
            };
            let keep = window.show(
                ctx,
                &mut self.layered,
                &scopes,
                &agent_rows,
                &self.config_dir,
                &mut self.keymap,
            );
            access_changes = std::mem::take(&mut window.access_changes);
            let open_drivers = std::mem::take(&mut window.open_drivers);
            let open_databases = std::mem::take(&mut window.open_databases);
            let open_folder = std::mem::take(&mut window.open_settings_folder);
            let copy_mcp = std::mem::take(&mut window.copy_mcp_config);
            let rotate = std::mem::take(&mut window.rotate_token);
            let open_terminal = std::mem::take(&mut window.open_terminal);
            let check_updates = std::mem::take(&mut window.check_updates);
            if open_drivers {
                self.drivers_ui.open_requested = true;
            }
            if open_databases {
                self.databases_dialog = Some(crate::databases_dialog::DatabasesDialog::matrix());
            }
            if open_folder {
                self.open_folder(&self.config_dir.clone());
            }
            if copy_mcp && let Some(config) = self.mcp.client_config() {
                ctx.copy_text(config);
                self.notify("MCP client configuration copied (it contains the token)");
            }
            if rotate {
                match self.mcp.rotate() {
                    Ok(()) => self.notify("MCP token rotated; active agents were disconnected"),
                    Err(error) => self.notify(format!("Could not rotate the token: {error}")),
                }
            }
            if open_terminal {
                self.terminal.open();
            }
            if check_updates {
                self.check_updates_now(ctx);
            }
            // Edits (and a Cancel) show at once: everything follows the live settings.
            self.settings = sqlr_store::Settings::resolve(&self.layered, None);
            self.apply_engine_settings();
            if !keep {
                self.prefs = None;
            }
        }
        for (id, access) in access_changes {
            self.set_agent_access(&id, access);
        }
        if let Some(target) = self.clear_history_pending.clone() {
            let (prompt, detail) = match &target {
                Some(id) => {
                    let name = self
                        .connection(id)
                        .map_or_else(|| id.clone(), |c| c.name.clone());
                    (
                        format!("Clear the query history of \"{name}\"?"),
                        Some("Its stored history entries are deleted; snippets are kept."),
                    )
                }
                None => (
                    "Clear the query history of every connection?".into(),
                    Some("Every stored history entry is deleted; snippets are kept."),
                ),
            };
            let ask = DeleteAsk {
                title: "Clear query history",
                action: "Delete",
                prompt: &prompt,
                detail,
            };
            match confirm_delete(ctx, ask) {
                Confirm::Pending => {}
                Confirm::Yes => {
                    self.engine.clear_history(target.as_deref());
                    self.clear_history_pending = None;
                }
                Confirm::No => self.clear_history_pending = None,
            }
        }
        if let Some(id) = self.delete_pending.clone() {
            let name = self
                .connection(&id)
                .map_or_else(|| id.clone(), |c| c.name.clone());
            let prompt = format!("Delete the connection \"{name}\"?");
            let ask = DeleteAsk {
                title: "Delete connection",
                action: "Delete",
                prompt: &prompt,
                detail: Some("Saved passwords for this connection will also be deleted."),
            };
            match confirm_delete(ctx, ask) {
                Confirm::Pending => {}
                Confirm::No => self.delete_pending = None,
                Confirm::Yes => {
                    self.delete_pending = None;
                    self.delete_connection(&id);
                }
            }
        }
        if let Some(name) = self.folder_delete_pending.clone() {
            let prompt = format!("Delete the folder \"{name}\"?");
            let ask = DeleteAsk {
                title: "Delete folder",
                action: "Delete",
                prompt: &prompt,
                detail: Some("Its connections move to the top level; none is deleted."),
            };
            match confirm_delete(ctx, ask) {
                Confirm::Pending => {}
                Confirm::No => self.folder_delete_pending = None,
                Confirm::Yes => {
                    self.folder_delete_pending = None;
                    self.delete_folder(&name);
                }
            }
        }
    }

    /// Remembers a run that waits for the connection's secrets.
    pub(crate) fn resume_after_secrets(
        &mut self,
        tab: u64,
        conn: &str,
        kind: crate::confirm::PendingKind,
    ) {
        if let Some(prompt) = &mut self.secret_prompt
            && prompt.conn == conn
        {
            prompt.resume = Some((tab, kind));
        }
    }

    /// The secrets prompt: "Connect" keeps them in memory and connects (then runs what was
    /// waiting); "Cancel" drops the connect.
    fn secret_prompt_ui(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.secret_prompt else {
            return;
        };
        let title = format!("Connect to {}", prompt.name);
        let response = theme::Dialog::new("secret_prompt", icons::KEY, &title)
            .subtitle("Secrets stay in memory until sqlr quits")
            .width(420.0)
            .show(ctx, |d| {
                d.body(|ui| {
                    for (key, label) in &prompt.fields {
                        let caption = ui.label(label);
                        let value = prompt.values.entry(key.clone()).or_default();
                        text_edit_secret(
                            ui,
                            egui::TextEdit::singleline(value)
                                .password(true)
                                .desired_width(f32::INFINITY),
                        )
                        .labelled_by(caption.id);
                    }
                });
                d.footer(|f| {
                    let connect = f.primary("Connect", None);
                    f.cancel();
                    connect
                })
            });
        let answer = if response.inner {
            Some(true)
        } else if response.closed {
            Some(false)
        } else {
            None
        };
        let Some(connect) = answer else {
            return;
        };
        let Some(prompt) = self.secret_prompt.take() else {
            return;
        };
        if !connect {
            return;
        }
        let secrets = Secrets(
            prompt
                .values
                .into_iter()
                .filter(|(_, v)| !v.is_empty())
                .collect(),
        );
        self.entered_secrets.insert(prompt.conn.clone(), secrets);
        if self.connect(&prompt.conn)
            && let Some((tab, kind)) = prompt.resume
        {
            self.start_kind(tab, &prompt.conn, kind);
        }
    }

    pub(crate) fn open_new_connection(&mut self) {
        self.open_new_connection_in(None);
    }

    /// Opens the new-connection dialog, optionally with a folder prefilled.
    pub(crate) fn open_new_connection_in(&mut self, folder: Option<&str>) {
        let manifests = self.engine.manifests();
        // PostgreSQL first when present; otherwise the first driver by name.
        let preferred = manifests
            .iter()
            .find(|m| m.id == "postgres")
            .or(manifests.first());
        match preferred {
            Some(manifest) => {
                self.replace_conn_dialog(ConnDialog::create_in(manifest, folder));
            }
            None => self.notify("No database drivers are available"),
        }
    }

    pub(crate) fn open_edit_connection(&mut self, id: &str) {
        let Some(cfg) = self.connection(id).cloned() else {
            return;
        };
        let Some(manifest) = self.manifest(&cfg.driver) else {
            self.notify(format!("Driver \"{}\" is not available", cfg.driver));
            return;
        };
        // A keychain error is shown inside the dialog, which then leaves the secrets alone.
        let dialog = ConnDialog::open_edit(&cfg, &manifest);
        self.replace_conn_dialog(dialog);
    }

    /// Saves a copy of `id` with its stored secrets, right after the original and in the same
    /// folder (on the store thread, which names it from the saved connections). The copy
    /// starts disconnected; a connection that keeps its secrets in memory only asks for them
    /// when it connects.
    pub(crate) fn duplicate_connection(&mut self, id: &str) {
        let Some(cfg) = self.connection(id).cloned() else {
            return;
        };
        let Some(manifest) = self.manifest(&cfg.driver) else {
            self.notify(format!("Driver \"{}\" is not available", cfg.driver));
            return;
        };
        self.store.send(StoreOp::Duplicate { cfg, manifest });
    }

    /// The store saved (or failed to save) the copy `copy` of the connection `name`.
    pub(crate) fn connection_duplicated(&mut self, name: &str, copy: Result<String, String>) {
        match copy {
            Ok(id) => {
                self.tree.selected = Some((id, NodePath::root()));
                self.tree.selected_folder = None;
            }
            Err(e) => self.notify(format!("Could not duplicate {name}: {e}")),
        }
    }

    fn replace_conn_dialog(&mut self, mut dialog: ConnDialog) {
        for op in dialog.take_ops() {
            self.store.send(op);
        }
        if let Some(mut old) = self.conn_dialog.replace(dialog) {
            old.finish(&self.engine);
        }
    }

    /// The store answered the connection dialog's save of `id` (`editing` a saved one).
    pub(crate) fn connection_save_ended(
        &mut self,
        id: &str,
        editing: bool,
        result: Result<(), String>,
    ) {
        match result {
            Ok(()) => {
                if let Some(mut dialog) = self.conn_dialog.take_if(|d| d.is_saving()) {
                    dialog.finish(&self.engine);
                }
                if editing {
                    self.connection_saved(id);
                }
            }
            Err(e) => {
                // A demo setup waiting for this save gives up.
                if id == crate::demo::DEMO_ID {
                    self.demo = None;
                }
                match &mut self.conn_dialog {
                    Some(dialog) if dialog.is_saving() => dialog.save_failed(id, &e),
                    _ => self.notify(format!("Could not save the connection: {e}")),
                }
            }
        }
    }

    pub(crate) fn delete_connection(&mut self, id: &str) {
        let Some(cfg) = self.connection(id).cloned() else {
            return;
        };
        let Some(manifest) = self.manifest(&cfg.driver) else {
            self.notify(format!("Driver \"{}\" is not available", cfg.driver));
            return;
        };
        // The store thread deletes it and its secrets; the explorer drops it at once (a
        // failed delete lists it again).
        self.store.send(StoreOp::Delete { cfg, manifest });
        self.connections.retain(|c| c.id != id);
        self.engine.disconnect(id);
        self.engine.invalidate_catalog(id);
        if self.tree.selected.as_ref().is_some_and(|(c, _)| c == id) {
            self.tree.selected = None;
        }
    }
}

/// A free name for a duplicate: `"<name> copy"`, then `"<name> copy 2"`, and so on.
pub(crate) fn copy_name(name: &str, taken: &[&str]) -> String {
    let free = |candidate: &str| !taken.iter().any(|n| n.eq_ignore_ascii_case(candidate));
    let first = format!("{name} copy");
    if free(&first) {
        return first;
    }
    (2..)
        .map(|n| format!("{name} copy {n}"))
        .find(|candidate| free(candidate))
        .expect("an unused name")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use sqlr_core::Driver;
    use sqlr_engine::DriverRegistry;
    use sqlr_store::{ConnectionRepo, SecretStore, StoreError};
    use sqlr_testkit::{FakeDriver, FakeScript};

    use super::*;

    #[test]
    fn sqlite_setup_starts_with_the_file_and_hides_password_controls() {
        use egui_kittest::kittest::Queryable;
        let driver = sqlr_driver_sqlite::SqliteDriver::new();
        let dialog = ConnDialog::create(driver.manifest());
        let mut drivers = DriverRegistry::default();
        drivers.register(driver);
        let engine = Engine::new(drivers, Arc::new(|| {}));
        let mut h = egui_kittest::Harness::builder()
            .with_size((1200.0, 900.0))
            .build_ui_state(
                |ui, dialog: &mut ConnDialog| {
                    dialog.show(ui.ctx(), &engine, &[], &[]);
                },
                dialog,
            );
        h.run_steps(3);
        assert!(
            h.query_by_label("Save password securely on this device")
                .is_none()
        );
        let file = h
            .get_all_by_label_contains("Database file")
            .next()
            .unwrap()
            .rect();
        assert!(file.top() < h.get_by_label("Name").rect().top());
    }

    #[test]
    fn duplicate_names_are_free_and_case_insensitive() {
        assert_eq!(copy_name("db", &["other"]), "db copy");
        assert_eq!(copy_name("db", &["db copy"]), "db copy 2");
        assert_eq!(copy_name("db", &["DB COPY", "db copy 2"]), "db copy 3");
    }

    /// A keychain whose reads fail; writes are recorded.
    #[derive(Clone, Default)]
    struct LockedKeychain {
        writes: Arc<Mutex<Vec<String>>>,
    }

    impl SecretStore for LockedKeychain {
        fn get(&self, _conn: &str, _field: &str) -> Result<Option<String>, StoreError> {
            Err(StoreError::Secret("keychain locked".into()))
        }
        fn set(&self, conn: &str, field: &str, _value: &str) -> Result<(), StoreError> {
            self.writes
                .lock()
                .unwrap()
                .push(format!("set {conn}/{field}"));
            Ok(())
        }
        fn delete(&self, conn: &str, field: &str) -> Result<(), StoreError> {
            self.writes
                .lock()
                .unwrap()
                .push(format!("delete {conn}/{field}"));
            Ok(())
        }
    }

    /// Runs the dialog's store requests on `repo` in place and hands it the answers.
    fn pump(dialog: &mut ConnDialog, repo: &sqlr_store::ConnectionRepo) {
        use crate::store_worker::{StoreReply, run};
        for op in dialog.take_ops() {
            match run(repo, op) {
                StoreReply::Secrets { id, result } => dialog.secrets_arrived(&id, result),
                StoreReply::Saved {
                    id, result: Err(e), ..
                } => dialog.save_failed(&id, &e),
                StoreReply::Saved { result: Ok(()), .. } => dialog.saving = false,
                _ => {}
            }
        }
    }

    #[test]
    fn failed_secret_read_then_save_keeps_stored_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = LockedKeychain::default();
        let repo = ConnectionRepo::open(dir.path(), Box::new(keychain.clone())).unwrap();
        let manifest = FakeDriver::new(FakeScript::default()).manifest().clone();
        let cfg = ConnectionConfig {
            id: "c1".into(),
            name: "old".into(),
            driver: manifest.id.clone(),
            group: None,
            color: None,
            values: [("host".to_string(), "h".to_string())].into(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        repo.save(&cfg, &Secrets::default()).unwrap();

        let mut dialog = ConnDialog::open_edit(&cfg, &manifest);
        assert!(dialog.error.is_none(), "nothing is read in place");
        pump(&mut dialog, &repo);
        assert!(dialog.error.as_deref().unwrap().contains("keychain locked"));
        dialog.form.name = "renamed".into();
        assert_eq!(dialog.save(&manifest), DialogEvent::None);
        assert!(dialog.is_saving());
        pump(&mut dialog, &repo);
        assert!(!dialog.is_saving());

        assert_eq!(repo.list().unwrap()[0].name, "renamed");
        assert!(
            keychain.writes.lock().unwrap().is_empty(),
            "the stored password must be left alone: {:?}",
            keychain.writes.lock().unwrap()
        );
    }

    #[test]
    fn switching_data_sources_keeps_unsaved_drafts() {
        let dir = tempfile::tempdir().unwrap();
        let repo = ConnectionRepo::open(
            dir.path(),
            Box::new(sqlr_store::MemorySecretStore::default()),
        )
        .unwrap();
        let manifest = FakeDriver::new(FakeScript::default()).manifest().clone();
        let engine = Engine::new(DriverRegistry::default(), Arc::new(|| {}));
        let cfg = ConnectionConfig {
            id: "c1".into(),
            name: "saved".into(),
            driver: manifest.id.clone(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        repo.save(&cfg, &Secrets::default()).unwrap();

        let mut dialog = ConnDialog::create(&manifest);
        dialog.form.name = "unsaved new".into();
        dialog.switch(&engine, Some((&cfg, &manifest)), Some(&manifest));
        assert_eq!(dialog.editing.as_deref(), Some("c1"));
        assert_eq!(dialog.form.name, "saved");
        dialog.form.name = "edited".into();

        dialog.switch(&engine, None, Some(&manifest));
        assert_eq!(dialog.editing, None);
        assert_eq!(dialog.form.name, "unsaved new");

        dialog.switch(&engine, Some((&cfg, &manifest)), Some(&manifest));
        assert_eq!(dialog.form.name, "edited");
    }

    #[test]
    fn file_dialog_starts_in_the_current_files_folder_without_touching_the_disk() {
        assert_eq!(
            start_dir("/no/such/dir/a.db"),
            Some(Path::new("/no/such/dir"))
        );
        assert_eq!(start_dir("a.db"), None);
        assert_eq!(start_dir(""), None);
    }

    #[test]
    fn file_dialog_only_for_file_fields() {
        let manifest = FakeDriver::new(FakeScript::default()).manifest().clone();
        assert!(PendingPick::open(&manifest, "host", "").is_none());
        assert!(PendingPick::open(&manifest, "missing", "").is_none());
    }

    #[test]
    fn emptied_stored_secret_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let repo = ConnectionRepo::open(
            dir.path(),
            Box::new(sqlr_store::MemorySecretStore::default()),
        )
        .unwrap();
        let manifest = FakeDriver::new(FakeScript::default()).manifest().clone();
        let mut dialog = ConnDialog::create(&manifest);
        dialog.form.name = "db".into();
        dialog
            .form
            .secrets
            .insert("password".into(), "hunter2".into());
        dialog.save(&manifest);
        pump(&mut dialog, &repo);
        let cfg = repo.list().unwrap().remove(0);
        assert_eq!(
            repo.secrets_for(&cfg, &manifest).unwrap().0["password"],
            "hunter2"
        );

        let mut dialog = ConnDialog::open_edit(&cfg, &manifest);
        pump(&mut dialog, &repo);
        assert!(dialog.error.is_none());
        assert_eq!(dialog.form.secrets["password"], "hunter2");
        dialog.form.secrets.remove("password");
        dialog.save(&manifest);
        pump(&mut dialog, &repo);
        assert!(repo.secrets_for(&cfg, &manifest).unwrap().0.is_empty());
    }
}
