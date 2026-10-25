//! Manifest-driven connection form.

use std::collections::BTreeMap;
use std::fmt;

use egui::{Color32, Sense, TextEdit, vec2};
use sqlr_core::{
    ConnectionConfig, DriverManifest, FieldSpec, FieldType, Secrets, parse_connection_url,
    redact_url,
};

use crate::theme::{self, Spacing};

const COLOR_PRESETS: [[u8; 3]; 6] = [
    [0xd9, 0x4f, 0x4f],
    [0xe0, 0x93, 0x3a],
    [0xd8, 0xc2, 0x4a],
    [0x5a, 0xb0, 0x5a],
    [0x4a, 0x8f, 0xd8],
    [0x9a, 0x6a, 0xd8],
];
/// The label column of the form, so every field starts at the same x.
const LABEL_WIDTH: f32 = 96.0;

/// The width a full-width field takes in its row: everything left after the label column.
fn field_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() - Spacing::XS).max(160.0)
}

#[derive(Clone, Debug, PartialEq)]
pub enum TestStatus {
    Running,
    Ok(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum FormAction {
    None,
    Test,
    Save,
    Cancel,
    /// "Browse…" on the `File` field `key`: the app shows a file dialog and fills it in.
    PickFile {
        key: String,
    },
}

/// Editable connection fields. Secret fields live in `secrets`, everything else in `values`.
#[derive(Clone)]
pub struct FormState {
    pub name: String,
    pub group: String,
    pub color: Option<[u8; 3]>,
    pub values: BTreeMap<String, String>,
    pub secrets: BTreeMap<String, String>,
    pub show_secrets: bool,
    /// The connection profile's id; `None` is the development profile.
    pub profile: Option<String>,
    /// Secrets go to the encrypted vault; when off they are asked for on connect.
    pub save_secrets: bool,
    /// Agent exposure. Human-only; the connection dialog is the only place that sets it.
    pub agent_access: sqlr_core::AgentAccess,
    /// The URL box; a URL holding a password is replaced by its redacted form.
    pub url: String,
    pub url_error: Option<String>,
    /// The field group shown when the manifest has several.
    pub tab: String,
    property_key: String,
    property_value: String,
}

/// `Debug` prints secret keys only, never their values.
impl fmt::Debug for FormState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FormState")
            .field("name", &self.name)
            .field("group", &self.group)
            .field("color", &self.color)
            .field("values", &self.values)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .field("show_secrets", &self.show_secrets)
            .field("profile", &self.profile)
            .field("save_secrets", &self.save_secrets)
            .field("url", &redact_url(&self.url))
            .field("tab", &self.tab)
            .finish()
    }
}

fn default_for(field: &FieldSpec) -> String {
    if field.group == "Properties" && field.default.is_none() {
        return String::new();
    }
    match (&field.default, &field.ty) {
        (Some(d), _) => d.clone(),
        (None, FieldType::Bool) => "false".into(),
        (None, FieldType::Choice(options)) => {
            options.first().map(|(v, _)| v.clone()).unwrap_or_default()
        }
        (None, _) => String::new(),
    }
}

/// Field groups in manifest order.
fn groups(manifest: &DriverManifest) -> Vec<&str> {
    let mut groups: Vec<&str> = Vec::new();
    for field in &manifest.fields {
        if !groups.contains(&field.group.as_str()) {
            groups.push(&field.group);
        }
    }
    groups
}

fn is_port_valid(value: &str) -> bool {
    value.parse::<u16>().is_ok_and(|p| p >= 1)
}

impl FormState {
    pub fn new(manifest: &DriverManifest) -> Self {
        let mut state = Self {
            name: String::new(),
            group: String::new(),
            color: None,
            values: BTreeMap::new(),
            secrets: BTreeMap::new(),
            show_secrets: false,
            profile: None,
            save_secrets: true,
            agent_access: sqlr_core::AgentAccess::None,
            url: String::new(),
            url_error: None,
            tab: String::new(),
            property_key: String::new(),
            property_value: String::new(),
        };
        for field in &manifest.fields {
            let default = default_for(field);
            if field.secret {
                if !default.is_empty() {
                    state.secrets.insert(field.key.clone(), default);
                }
            } else {
                state.values.insert(field.key.clone(), default);
            }
        }
        state
    }

    /// Fills the fields a known database presets (`(key, value)`, keys of `manifest`'s
    /// non-secret fields; others are ignored).
    pub fn apply_preset(&mut self, manifest: &DriverManifest, preset: &[(&str, &str)]) {
        for (key, value) in preset {
            if manifest.fields.iter().any(|f| !f.secret && f.key == *key) {
                self.values.insert(key.to_string(), value.to_string());
            }
        }
    }

    pub fn from_config(
        cfg: &ConnectionConfig,
        secrets: &Secrets,
        manifest: &DriverManifest,
    ) -> Self {
        let mut state = Self::new(manifest);
        state.name = cfg.name.clone();
        state.group = cfg.group.clone().unwrap_or_default();
        state.color = cfg.color;
        state.profile = cfg.profile.clone();
        state.save_secrets = cfg.save_secrets;
        state.agent_access = cfg.agent_access;
        state.secrets.clear();
        for field in &manifest.fields {
            if field.secret {
                if let Some(v) = secrets.0.get(&field.key) {
                    state.secrets.insert(field.key.clone(), v.clone());
                }
            } else if let Some(v) = cfg.value(manifest, &field.key) {
                state.values.insert(field.key.clone(), v.to_string());
            }
        }
        if manifest
            .fields
            .iter()
            .any(|f| f.group == "Properties" && f.free_form)
        {
            state.values.extend(
                cfg.values
                    .iter()
                    .filter(|(k, _)| k.starts_with("property."))
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
        }
        state
    }

    fn field_value(&self, field: &FieldSpec) -> &str {
        let map = if field.secret {
            &self.secrets
        } else {
            &self.values
        };
        map.get(&field.key).map_or("", String::as_str)
    }

    pub fn validate(&self, manifest: &DriverManifest) -> Vec<String> {
        let mut errors = Vec::new();
        if self.name.trim().is_empty() {
            errors.push("Name is required".to_string());
        }
        for field in &manifest.fields {
            let value = self.field_value(field).trim();
            if value.is_empty() {
                if field.required {
                    errors.push(format!("{} is required", field.label));
                }
            } else {
                match &field.ty {
                    FieldType::Port if !is_port_valid(value) => {
                        errors.push(format!("{} must be 1–65535", field.label));
                    }
                    FieldType::Number if value.parse::<u64>().is_err() => {
                        errors.push(format!("{} must be a whole number", field.label));
                    }
                    FieldType::Choice(options) if !options.iter().any(|(v, _)| v == value) => {
                        errors.push(format!("{} has an unknown value", field.label));
                    }
                    _ => {}
                }
            }
        }
        let properties = self
            .values
            .iter()
            .filter(|(k, v)| k.starts_with("property.") && !v.is_empty())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Err(error) = sqlr_core::validate_driver_properties(&manifest.fields, &properties) {
            errors.push(error.to_string());
        }
        errors
    }

    /// Secret fields go to `Secrets` only, so they can never reach `connections.toml`.
    pub fn to_config(&self, id: String, manifest: &DriverManifest) -> (ConnectionConfig, Secrets) {
        let mut values = BTreeMap::new();
        let mut secrets = BTreeMap::new();
        for field in &manifest.fields {
            let value = self.field_value(field).trim().to_string();
            if field.secret {
                if !value.is_empty() {
                    secrets.insert(field.key.clone(), self.field_value(field).to_string());
                }
            } else if field.group != "Properties" || !value.is_empty() {
                values.insert(field.key.clone(), value);
            }
        }
        if manifest
            .fields
            .iter()
            .any(|f| f.group == "Properties" && f.free_form)
        {
            values.extend(
                self.values
                    .iter()
                    .filter(|(k, v)| k.starts_with("property.") && !v.is_empty())
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
        }
        let group = self.group.trim();
        let config = ConnectionConfig {
            id,
            name: self.name.trim().to_string(),
            driver: manifest.id.clone(),
            group: (!group.is_empty()).then(|| group.to_string()),
            color: self.color,
            values,
            profile: self.profile.clone(),
            save_secrets: self.save_secrets,
            agent_access: self.agent_access,
        };
        (config, Secrets(secrets))
    }

    /// Fills the fields from `self.url`, or sets `url_error`.
    fn apply_url(&mut self, manifest: &DriverManifest) {
        let text = self.url.trim().to_string();
        if text.is_empty() {
            self.url_error = None;
            return;
        }
        match parse_connection_url(&text) {
            Err(e) => {
                // Never keep a typed password around.
                self.url = redact_url(&text);
                self.url_error = Some(e);
            }
            Ok(url) if !manifest.url_schemes.contains(&url.scheme) => {
                let schemes: Vec<String> = manifest
                    .url_schemes
                    .iter()
                    .map(|s| format!("{s}://"))
                    .collect();
                self.url_error = Some(format!(
                    "This connection takes {} URLs",
                    schemes.join(" or ")
                ));
            }
            Ok(url) => {
                url.apply(manifest, &mut self.values);
                self.url_error = None;
            }
        }
    }
}

/// A row of the form: a fixed-width caption on the left, the field on the right, as in the
/// Connection dialog.
fn labeled(ui: &mut egui::Ui, caption: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::M;
        ui.allocate_ui_with_layout(
            vec2(LABEL_WIDTH, ui.spacing().interact_size.y),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(LABEL_WIDTH);
                ui.label(caption);
            },
        );
        add(ui);
    });
}

/// A field with its caption on the left; a checkbox keeps the caption in the label column so
/// the column stays aligned.
fn field_edit(ui: &mut egui::Ui, state: &mut FormState, field: &FieldSpec) -> FormAction {
    let caption = if field.required {
        format!("{} *", field.label)
    } else {
        field.label.clone()
    };
    let show_secrets = state.show_secrets;
    let map = if field.secret {
        &mut state.secrets
    } else {
        &mut state.values
    };
    let value = map.entry(field.key.clone()).or_default();
    let mut action = FormAction::None;
    match &field.ty {
        FieldType::Bool => {
            labeled(ui, &caption, |ui| {
                let mut checked = value == "true";
                if ui.checkbox(&mut checked, "").changed() {
                    *value = checked.to_string();
                }
            });
        }
        FieldType::Port | FieldType::Number => labeled(ui, &caption, |ui| {
            let response =
                crate::text_menu::text_edit(ui, TextEdit::singleline(value).desired_width(96.0));
            if response.changed() {
                value.retain(|c| c.is_ascii_digit());
                if field.ty == FieldType::Port {
                    value.truncate(5);
                }
            }
        }),
        FieldType::Password => labeled(ui, &caption, |ui| {
            crate::text_menu::text_edit_secret(
                ui,
                TextEdit::singleline(value)
                    .password(!show_secrets)
                    .desired_width(field_width(ui)),
            );
        }),
        FieldType::File { .. } => labeled(ui, &caption, |ui| {
            ui.horizontal(|ui| {
                let width = field_width(ui);
                crate::text_menu::text_edit(
                    ui,
                    TextEdit::singleline(value).desired_width(width - 88.0),
                );
                if ui.button("Browse…").clicked() {
                    action = FormAction::PickFile {
                        key: field.key.clone(),
                    };
                }
            });
        }),
        FieldType::Choice(options) => {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = Spacing::M;
                let label = ui
                    .allocate_ui_with_layout(
                        vec2(LABEL_WIDTH, ui.spacing().interact_size.y),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_min_width(LABEL_WIDTH);
                            ui.label(&caption)
                        },
                    )
                    .inner;
                let selected = options
                    .iter()
                    .find(|(v, _)| v == value)
                    .map_or(value.as_str(), |(_, l)| l.as_str())
                    .to_string();
                egui::ComboBox::from_id_salt(("connection_form_choice", &field.key))
                    .selected_text(selected)
                    .width(field_width(ui) - 8.0)
                    .show_ui(ui, |ui| {
                        for (v, l) in options {
                            ui.selectable_value(value, v.clone(), l);
                        }
                    })
                    .response
                    .labelled_by(label.id);
            });
        }
        FieldType::Text => labeled(ui, &caption, |ui| {
            let edit = TextEdit::singleline(value).desired_width(field_width(ui));
            if field.secret && !show_secrets {
                crate::text_menu::text_edit_secret(ui, edit.password(true));
            } else {
                crate::text_menu::text_edit(ui, edit);
            }
        }),
    }
    action
}

/// The URL box: pasting or typing a URL fills the fields it names.
fn url_box(ui: &mut egui::Ui, state: &mut FormState, manifest: &DriverManifest) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::M;
        let label = ui
            .allocate_ui_with_layout(
                vec2(LABEL_WIDTH, ui.spacing().interact_size.y),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(LABEL_WIDTH);
                    ui.label("URL")
                },
            )
            .inner;
        let hint = manifest.url_schemes.first().map_or_else(String::new, |s| {
            if manifest.default_port.is_some() {
                format!("{s}://user@host:port/database")
            } else {
                format!("{s}:/path/to/file")
            }
        });
        let width = field_width(ui);
        let response = crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut state.url)
                .hint_text(hint)
                .desired_width(width),
        )
        .labelled_by(label.id);
        if response.changed() {
            state.apply_url(manifest);
        }
    });
    if let Some(error) = &state.url_error {
        ui.colored_label(theme::palette(ui.ctx()).error, error);
    }
}

fn color_picker(ui: &mut egui::Ui, color: &mut Option<[u8; 3]>) {
    ui.horizontal(|ui| {
        if ui
            .selectable_label(color.is_none(), "None")
            .on_hover_text("No color")
            .clicked()
        {
            *color = None;
        }
        for preset in COLOR_PRESETS {
            let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
            let fill = Color32::from_rgb(preset[0], preset[1], preset[2]);
            ui.painter().circle_filled(rect.center(), 7.0, fill);
            if *color == Some(preset) {
                ui.painter().circle_stroke(
                    rect.center(),
                    9.0,
                    egui::Stroke::new(1.5, ui.visuals().strong_text_color()),
                );
            }
            if response.clicked() {
                *color = Some(preset);
            }
        }
    });
}

/// Name, group and color, the three fields every driver has.
pub fn connection_identity(ui: &mut egui::Ui, state: &mut FormState) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = Spacing::M;
        labeled(ui, "Name", |ui| {
            crate::text_menu::text_edit(
                ui,
                TextEdit::singleline(&mut state.name)
                    .hint_text("e.g. Development database")
                    .desired_width(field_width(ui)),
            );
        });
        labeled(ui, "Folder", |ui| {
            crate::text_menu::text_edit(
                ui,
                TextEdit::singleline(&mut state.group)
                    .hint_text("Optional")
                    .desired_width(field_width(ui)),
            );
        });
        labeled(ui, "Color", |ui| color_picker(ui, &mut state.color));
    });
}

/// The driver-specific part of the form: the group tabs, the URL box, the fields and their
/// validation errors. The caller draws the identity rows and the action buttons.
pub fn connection_form_body(
    ui: &mut egui::Ui,
    state: &mut FormState,
    manifest: &DriverManifest,
) -> FormAction {
    let mut action = FormAction::None;

    // Several field groups become tabs (Main, SSH, TLS, Advanced, …).
    let groups = groups(manifest);
    if !groups.iter().any(|g| *g == state.tab) {
        state.tab = groups.first().map(|g| g.to_string()).unwrap_or_default();
    }
    if groups.len() > 1 {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::S;
            for group in &groups {
                if theme::dialog_tab(ui, state.tab == *group, group).clicked() {
                    state.tab = group.to_string();
                }
            }
        });
        theme::hairline(ui);
    }
    ui.add_space(Spacing::M);
    if !manifest.url_schemes.is_empty() && groups.first().is_none_or(|g| *g == state.tab) {
        url_box(ui, state, manifest);
        ui.add_space(Spacing::M);
    }
    let tab = state.tab.clone();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = Spacing::M;
        for field in manifest.fields.iter().filter(|f| f.group == tab) {
            let a = field_edit(ui, state, field);
            if a != FormAction::None {
                action = a;
            }
        }
    });

    if tab == "Properties"
        && manifest
            .fields
            .iter()
            .any(|f| f.group == "Properties" && f.free_form)
    {
        let extras: Vec<_> = state
            .values
            .keys()
            .filter(|k| k.starts_with("property.") && !manifest.fields.iter().any(|f| f.key == **k))
            .cloned()
            .collect();
        for key in extras {
            ui.horizontal(|ui| {
                ui.label(key.trim_start_matches("property."));
                if let Some(value) = state.values.get_mut(&key) {
                    crate::text_edit(ui, TextEdit::singleline(value));
                }
                if ui.small_button("Remove").clicked() {
                    state.values.remove(&key);
                }
            });
        }
        ui.horizontal(|ui| {
            crate::text_edit(
                ui,
                TextEdit::singleline(&mut state.property_key).hint_text("Property key"),
            );
            crate::text_edit(
                ui,
                TextEdit::singleline(&mut state.property_value).hint_text("Property value"),
            );
            if ui
                .add_enabled(
                    !state.property_key.trim().is_empty(),
                    egui::Button::new("Add property"),
                )
                .clicked()
            {
                state.values.insert(
                    format!("property.{}", state.property_key.trim()),
                    std::mem::take(&mut state.property_value),
                );
                state.property_key.clear();
            }
        });
    }

    if manifest.fields.iter().any(|f| f.secret) {
        ui.add_space(Spacing::S);
        labeled(ui, "", |ui| {
            ui.checkbox(&mut state.show_secrets, "Show secrets");
        });
    }

    // Missing required fields are marked with "*" and named beside Save; only values that
    // are wrong as typed are listed here, so a fresh form does not open on a wall of red.
    let errors: Vec<String> = state
        .validate(manifest)
        .into_iter()
        .filter(|e| !e.ends_with(" is required"))
        .collect();
    if !errors.is_empty() {
        ui.add_space(Spacing::M);
        let error = theme::palette(ui.ctx()).error;
        for message in &errors {
            ui.horizontal(|ui| {
                ui.add_space(LABEL_WIDTH + Spacing::M);
                ui.colored_label(error, message);
            });
        }
    }
    action
}

/// "Testing…", the server version or the failure of the last connection test.
pub fn connection_form_status(ui: &mut egui::Ui, test: Option<&TestStatus>) {
    let palette = theme::palette(ui.ctx());
    match test {
        Some(TestStatus::Running) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Testing…");
            });
        }
        Some(TestStatus::Ok(version)) => {
            ui.colored_label(palette.success, format!("Connected: {version}"));
        }
        Some(TestStatus::Failed(message)) => {
            ui.colored_label(palette.error, message);
        }
        None => {}
    }
}

pub fn connection_form(
    ui: &mut egui::Ui,
    state: &mut FormState,
    manifest: &DriverManifest,
    test: Option<&TestStatus>,
) -> FormAction {
    connection_identity(ui, state);
    ui.add_space(Spacing::M);
    let mut action = connection_form_body(ui, state, manifest);
    let errors = state.validate(manifest);
    ui.add_space(Spacing::S);
    connection_form_status(ui, test);

    ui.add_space(Spacing::L);
    ui.horizontal(|ui| {
        if ui.button("Test").clicked() {
            action = FormAction::Test;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let save = theme::primary_button_state(ui.ctx(), errors.is_empty(), "Save");
            if ui.add_enabled(errors.is_empty(), save).clicked() {
                action = FormAction::Save;
            }
            if ui.button("Cancel").clicked() {
                action = FormAction::Cancel;
            }
            // Why Save is disabled, beside it.
            if let Some(reason) = errors.first() {
                theme::hint(ui, reason);
            }
        });
    });
    action
}
