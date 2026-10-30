//! Controls of the preferences window: one editor per setting type and the marker of an
//! overridden value. They edit a [`SettingValue`] and know nothing about where it is stored.

use egui::{DragValue, RichText, Ui};
use sqlr_core::{SettingDef, SettingKind, SettingValue};

use crate::theme::{self, Spacing, icons};

/// Edits `value` with the control the setting's type calls for. Returns whether it changed.
pub fn setting_editor(ui: &mut Ui, def: &SettingDef, value: &mut SettingValue) -> bool {
    let mut changed = false;
    match (def.kind, &mut *value) {
        (SettingKind::Bool, SettingValue::Bool(b)) => {
            // The label belongs to the box, so a screen reader names it.
            changed |= ui.checkbox(b, def.label).changed();
        }
        (SettingKind::Int { min, max }, SettingValue::Int(i)) => {
            let limited = def.id == "grid.row_cap" || def.id.ends_with("memory_mib");
            let id = ui.make_persistent_id(("last_limit", def.id));
            let mut unlimited = limited && *i == 0;
            let speed = ((*i).unsigned_abs() as f64 * 0.01).max(1.0);
            changed |= ui
                .add_enabled_ui(!unlimited, |ui| {
                    ui.add_sized(
                        egui::vec2(100.0, ui.spacing().interact_size.y),
                        DragValue::new(i).range(min..=max).speed(speed),
                    )
                })
                .inner
                .changed();
            if limited && ui.checkbox(&mut unlimited, "No limit").changed() {
                if unlimited {
                    ui.ctx().data_mut(|d| d.insert_temp(id, *i));
                    *i = 0;
                } else {
                    *i = ui
                        .ctx()
                        .data(|d| d.get_temp::<i64>(id))
                        .unwrap_or_else(|| def.default.as_i64().max(1));
                }
                changed = true;
            }
            if def.id.ends_with("memory_mib") {
                ui.label("MiB")
                    .on_hover_text("One mebibyte is 1,048,576 bytes");
            }
        }
        (SettingKind::Float { min, max, step }, SettingValue::Float(f)) => {
            if def.id == "appearance.ui_scale" {
                // The usual zoom steps, then the exact value: tighter gaps keep them on one
                // line within the settings row.
                ui.spacing_mut().item_spacing.x = Spacing::S;
                for pct in [75, 100, 125, 150, 200] {
                    let scale = f64::from(pct) / 100.0;
                    let selected = (*f - scale).abs() < 0.001;
                    if theme::segment(ui, selected, &format!("{pct}%")).clicked() {
                        *f = scale;
                        changed = true;
                    }
                }
            }
            if def.id == "appearance.ui_scale" {
                ui.label("Custom");
            }
            let decimals = if step >= 1.0 {
                0
            } else if step < 0.1 {
                2
            } else {
                1
            };
            changed |= ui
                .add(
                    DragValue::new(f)
                        .range(min..=max)
                        .speed(step)
                        .fixed_decimals(decimals),
                )
                .changed();
        }
        (SettingKind::Text, SettingValue::Text(s))
        | (SettingKind::Path { .. }, SettingValue::Path(s)) => {
            if def.id.starts_with("drivers.") && def.id.ends_with(".properties") {
                changed |= properties_editor(ui, def.id, s);
            } else if def.id.starts_with("formats.") {
                let (placeholder, presets): (&str, &[(&str, &str)]) = match def.id {
                    "formats.date" => (
                        "Server format (2026-10-25)",
                        &[
                            ("ISO · 2026-10-25", "%Y-%m-%d"),
                            ("European · 25/10/2026", "%d/%m/%Y"),
                            ("US · 10/25/2026", "%m/%d/%Y"),
                        ],
                    ),
                    "formats.time" => (
                        "Server format (14:35:20)",
                        &[
                            ("24 hour · 14:35:20", "%H:%M:%S"),
                            ("Short · 14:35", "%H:%M"),
                            ("12 hour · 02:35 PM", "%I:%M %p"),
                        ],
                    ),
                    "formats.timestamptz" => (
                        "Server format (UTC)",
                        &[
                            ("ISO with zone", "%Y-%m-%d %H:%M:%S %:z"),
                            ("European with zone", "%d/%m/%Y %H:%M:%S %:z"),
                        ],
                    ),
                    _ => (
                        "Server format (2026-10-25 14:35:20)",
                        &[
                            ("ISO date and time", "%Y-%m-%d %H:%M:%S"),
                            ("European date and time", "%d/%m/%Y %H:%M:%S"),
                        ],
                    ),
                };
                changed |= crate::text_edit(
                    ui,
                    egui::TextEdit::singleline(s)
                        .hint_text(placeholder)
                        .desired_width(170.0),
                )
                .changed();
                let response = egui::ComboBox::from_id_salt(("format_preset", def.id))
                    .selected_text("Presets")
                    .width(78.0)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(s.is_empty(), "Server format").clicked() {
                            s.clear();
                            changed = true;
                        }
                        for (name, pattern) in presets {
                            if ui.selectable_label(s == pattern, *name).clicked() {
                                *s = pattern.to_string();
                                changed = true;
                            }
                        }
                    })
                    .response;
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::ComboBox,
                        true,
                        if def.id == "formats.date" {
                            "Date presets"
                        } else {
                            "Format presets"
                        },
                    )
                });
            } else {
                changed |= crate::text_edit(ui, egui::TextEdit::singleline(s).desired_width(200.0))
                    .changed();
            }
        }
        (SettingKind::Choice(options), SettingValue::Choice(current)) => {
            for (id, label) in options {
                if theme::segment(ui, current == id, label).clicked() && current != id {
                    *current = (*id).to_string();
                    changed = true;
                }
            }
        }
        (SettingKind::Color, SettingValue::Color(c)) => {
            changed |= ui.color_edit_button_srgb(c).changed();
        }
        // A value of the wrong type is replaced by the default, which the caller stores.
        _ => {
            *value = def.default.clone();
            changed = true;
        }
    }
    changed
}

/// The dot beside a value that is set at the level being edited.
pub fn override_dot(ui: &mut Ui, tooltip: &str) {
    let p = theme::palette(ui.ctx());
    ui.label(RichText::new(icons::DOT).size(9.0).color(p.accent))
        .on_hover_text(tooltip);
}

#[derive(Clone)]
struct PropertyDraft {
    source: String,
    pending: Option<String>,
    rows: Vec<(String, String)>,
}

fn properties_editor(ui: &mut Ui, key: &str, text: &mut String) -> bool {
    let id = ui.make_persistent_id(("property_editor", key));
    let stored = ui.ctx().data_mut(|d| d.get_temp::<PropertyDraft>(id));
    let mut draft = match stored {
        Some(draft) if draft.source == *text || draft.pending.as_deref() == Some(text.as_str()) => {
            draft
        }
        _ => {
            let rows = serde_json::from_str::<std::collections::BTreeMap<String, String>>(text)
                .unwrap_or_default()
                .into_iter()
                .collect();
            PropertyDraft {
                source: text.clone(),
                pending: None,
                rows,
            }
        }
    };
    if draft.pending.as_deref() == Some(text.as_str()) {
        draft.source = text.clone();
        draft.pending = None;
    }
    let mut changed = false;
    let mut remove = None;
    ui.vertical(|ui| {
        for (index, (name, value)) in draft.rows.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                changed |= crate::text_edit(
                    ui,
                    egui::TextEdit::singleline(name)
                        .id_salt((id, index, "key"))
                        .hint_text("Property")
                        .desired_width(115.0),
                )
                .changed();
                changed |= crate::text_edit(
                    ui,
                    egui::TextEdit::singleline(value)
                        .id_salt((id, index, "value"))
                        .hint_text("Value")
                        .desired_width(145.0),
                )
                .changed();
                if icons::icon_button(ui, icons::DELETE, None, "Remove property").clicked() {
                    remove = Some(index);
                }
            });
        }
        if ui.button("Add property").clicked() {
            draft.rows.push((String::new(), String::new()));
        }
    });
    if let Some(index) = remove {
        draft.rows.remove(index);
        changed = true;
    }
    let mut names = std::collections::HashSet::new();
    let valid = draft
        .rows
        .iter()
        .all(|(key, _)| !key.trim().is_empty() && names.insert(key.trim()));
    if changed && valid {
        let map: std::collections::BTreeMap<_, _> = draft
            .rows
            .iter()
            .map(|(k, v)| (k.trim(), v.as_str()))
            .collect();
        *text = serde_json::to_string(&map).expect("string properties serialize");
        draft.pending = Some(text.clone());
    } else if !valid && draft.rows.iter().any(|(k, _)| !k.is_empty()) {
        ui.colored_label(
            theme::palette(ui.ctx()).warning,
            "Use a unique name for each property",
        );
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, draft));
    changed && valid
}
