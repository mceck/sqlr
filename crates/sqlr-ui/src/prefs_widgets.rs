//! Controls of the preferences window: one editor per setting type and the marker of an
//! overridden value. They edit a [`SettingValue`] and know nothing about where it is stored.

use egui::{DragValue, RichText, Ui};
use sqlr_core::{SettingDef, SettingKind, SettingValue};

use crate::theme::{self, icons};

/// Edits `value` with the control the setting's type calls for. Returns whether it changed.
pub fn setting_editor(ui: &mut Ui, def: &SettingDef, value: &mut SettingValue) -> bool {
    let mut changed = false;
    match (def.kind, &mut *value) {
        (SettingKind::Bool, SettingValue::Bool(b)) => {
            // The label belongs to the box, so a screen reader names it.
            changed |= ui.checkbox(b, def.label).changed();
        }
        (SettingKind::Int { min, max }, SettingValue::Int(i)) => {
            let speed = ((*i).unsigned_abs() as f64 * 0.01).max(1.0);
            changed |= ui
                .add(DragValue::new(i).range(min..=max).speed(speed))
                .changed();
        }
        (SettingKind::Float { min, max, step }, SettingValue::Float(f)) => {
            if def.id == "appearance.ui_scale" {
                // The usual zoom steps, then the exact value.
                for pct in [75, 100, 125, 150, 200] {
                    let scale = f64::from(pct) / 100.0;
                    let selected = (*f - scale).abs() < 0.001;
                    if theme::segment(ui, selected, &format!("{pct}%")).clicked() {
                        *f = scale;
                        changed = true;
                    }
                }
            }
            let decimals = if step < 0.1 { 2 } else { 1 };
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
            changed |=
                crate::text_edit(ui, egui::TextEdit::singleline(s).desired_width(260.0)).changed();
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
