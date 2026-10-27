//! The rows of the preferences window: one per setting, with where its value comes from.

use egui::{Align, Layout, RichText};
use sqlr_core::{Level, Levels, Safety, SettingDef, SettingKind, SettingValue};
use sqlr_store::LayeredSettings;
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{override_dot, setting_editor};

use super::resolve::Scopes;

/// Width of the label column of the rows that have one.
const LABEL_WIDTH: f32 = 230.0;

/// What the rows edit: the live settings at one level.
pub(super) struct Editing<'a> {
    pub live: &'a mut LayeredSettings,
    pub scope: &'a Level,
    pub scopes: &'a Scopes,
    pub error: &'a mut Option<String>,
}

/// The group title a setting is listed under inside its section ("" for none).
fn group_of(def: &SettingDef) -> &'static str {
    match def.section {
        "General" if def.id.starts_with("updates.") => "Updates",
        "General" if def.id.starts_with("diagnostics.") => "Privacy & diagnostics",
        "General" if def.id.starts_with("operations.") => "Operations",
        "General" => "Execution",
        "Drivers" if def.id.starts_with("drivers.postgres.") => "PostgreSQL",
        "Drivers" if def.id.starts_with("drivers.mysql.") => "MySQL",
        "Drivers" if def.id.starts_with("drivers.sqlite.") => "SQLite",
        "Drivers" => "All drivers",
        _ => "",
    }
}

impl Editing<'_> {
    /// Whether the setting can be edited at the level being edited.
    pub(super) fn applies(&self, def: &SettingDef) -> bool {
        *self.scope == Level::Global || def.levels == Levels::All
    }

    /// The rows of `defs` that apply here, under their group titles. The rows of a group are
    /// kept together, in the order the group first appears.
    pub(super) fn rows(&mut self, ui: &mut egui::Ui, defs: &[&'static SettingDef]) {
        let mut groups: Vec<&str> = Vec::new();
        for def in defs {
            let g = group_of(def);
            if !groups.contains(&g) {
                groups.push(g);
            }
        }
        // Updates go last in General, next to the "Check for updates now" that follows them.
        if let Some(i) = groups.iter().position(|g| *g == "Updates") {
            let updates = groups.remove(i);
            groups.push(updates);
        }
        let mut ordered: Vec<&'static SettingDef> = defs.to_vec();
        ordered.sort_by_key(|def| groups.iter().position(|g| *g == group_of(def)));
        let mut group = "";
        for def in ordered {
            let next = group_of(def);
            if next != group {
                group = next;
                ui.add_space(Spacing::M);
                theme::section_title(ui, group);
            }
            if self.applies(def) {
                self.row(ui, def);
            }
        }
    }

    fn row(&mut self, ui: &mut egui::Ui, def: &'static SettingDef) {
        let key = self.scopes.resolve_key(self.scope);
        let (effective, from) = self.live.get(def.id, key);
        // At the global level a value is "set" when it differs from the default.
        let overridden = self
            .live
            .value_at(def.id, self.scope)
            .is_some_and(|v| *self.scope != Level::Global || *v != def.default);
        // A flag a higher level switched on cannot be loosened here.
        let locked = def.safety == Safety::TightenOnly
            && *self.scope != Level::Global
            && effective.as_bool()
            && from != *self.scope;

        let mut edited = effective;
        let mut changed = false;
        let mut reset = false;
        ui.add_space(Spacing::XS);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::M;
            if overridden {
                let tip = match self.scope {
                    Level::Global => "Changed from the default".to_string(),
                    scope => format!("Set for {}", self.scopes.name(scope)),
                };
                override_dot(ui, &tip);
            } else {
                ui.add_space(9.0 + ui.spacing().item_spacing.x);
            }
            if def.kind != SettingKind::Bool {
                ui.allocate_ui_with_layout(
                    egui::vec2(LABEL_WIDTH, ui.spacing().interact_size.y),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_min_width(LABEL_WIDTH);
                        ui.add(egui::Label::new(def.label).truncate())
                            .on_hover_text(def.label);
                    },
                );
            }
            ui.add_enabled_ui(!locked, |ui| {
                changed = setting_editor(ui, def, &mut edited);
            });
            if overridden
                && ui
                    .small_button("Reset")
                    .on_hover_text("Clear the value")
                    .clicked()
            {
                reset = true;
            }
            if !overridden && *self.scope != Level::Global {
                let from = self.scopes.name(&from);
                theme::hint(ui, &format!("from {from}"));
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(9.0 + 2.0 * ui.spacing().item_spacing.x);
            ui.add(egui::Label::new(RichText::new(def.description).small().weak()).wrap());
        });
        if def.id.starts_with("formats.") {
            let formats = sqlr_core::DataFormats::from_values(|id| {
                if id == def.id {
                    Some(edited.clone())
                } else {
                    Some(self.live.get(id, self.scopes.resolve_key(self.scope)).0)
                }
            });
            let (kind, type_name, hint, sample) = match def.id {
                "formats.date" => (
                    sqlr_core::ColumnKind::Text,
                    "date",
                    sqlr_core::DisplayHint::Temporal,
                    sqlr_core::Cell::Text("2026-10-25"),
                ),
                "formats.time" => (
                    sqlr_core::ColumnKind::Text,
                    "time",
                    sqlr_core::DisplayHint::Temporal,
                    sqlr_core::Cell::Text("14:35:20"),
                ),
                "formats.timestamp" => (
                    sqlr_core::ColumnKind::Text,
                    "timestamp",
                    sqlr_core::DisplayHint::Temporal,
                    sqlr_core::Cell::Text("2026-10-25 14:35:20"),
                ),
                "formats.timestamptz" => (
                    sqlr_core::ColumnKind::Text,
                    "timestamptz",
                    sqlr_core::DisplayHint::Temporal,
                    sqlr_core::Cell::Text("2026-10-25T14:35:20Z"),
                ),
                "formats.binary" => (
                    sqlr_core::ColumnKind::Bytes,
                    "binary",
                    sqlr_core::DisplayHint::Binary,
                    sqlr_core::Cell::Bytes(&[0x12, 0xab]),
                ),
                "formats.json" => (
                    sqlr_core::ColumnKind::Text,
                    "json",
                    sqlr_core::DisplayHint::Json,
                    sqlr_core::Cell::Text("{\"name\":\"sqlr\"}"),
                ),
                "formats.boolean" => (
                    sqlr_core::ColumnKind::Bool,
                    "boolean",
                    sqlr_core::DisplayHint::Plain,
                    sqlr_core::Cell::Bool(true),
                ),
                _ => (
                    sqlr_core::ColumnKind::Float,
                    "float",
                    sqlr_core::DisplayHint::Number,
                    sqlr_core::Cell::Float(1234567.89),
                ),
            };
            if def.id != "formats.use_for_export" {
                let meta = sqlr_core::ColumnMeta::new("sample", type_name, kind, hint);
                ui.horizontal(|ui| {
                    ui.add_space(9.0 + 2.0 * Spacing::M);
                    theme::hint(ui, &format!("Preview → {}", formats.format(&meta, sample)));
                });
            }
        }
        ui.add_space(Spacing::S);

        if reset {
            self.store(def, None);
        } else if changed {
            self.store(def, Some(edited));
        }
    }

    fn store(&mut self, def: &'static SettingDef, value: Option<SettingValue>) {
        // Below the global level a flag that is not on is no override at all.
        let value = match value {
            Some(v)
                if def.safety == Safety::TightenOnly
                    && *self.scope != Level::Global
                    && !v.as_bool() =>
            {
                None
            }
            v => v,
        };
        *self.error = self
            .live
            .set(def.id, self.scope, value)
            .err()
            .map(|e| format!("Could not change the setting: {e}"));
    }
}
