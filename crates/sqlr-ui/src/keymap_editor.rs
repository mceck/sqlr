//! Search, record and reset action bindings.
use sqlr_core::{ActionId, Keymap, PRESETS};
#[derive(Default)]
pub struct KeymapEditor {
    query: String,
    recording: Option<ActionId>,
}
impl KeymapEditor {
    pub fn show(&mut self, ui: &mut egui::Ui, map: &mut Keymap, preset: &mut String) {
        let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
        ui.horizontal(|ui| {
            ui.label("Preset");
            egui::ComboBox::from_id_salt("keymap_preset")
                .selected_text(preset.as_str())
                .show_ui(ui, |ui| {
                    for (id, label) in PRESETS {
                        if ui.selectable_label(preset == id, *label).clicked() {
                            *preset = id.to_string();
                            *map = Keymap::preset(id).unwrap_or_default();
                        }
                    }
                });
            if ui.button("Reset all").clicked() {
                *map = Keymap::preset(preset).unwrap_or_default();
            }
        });
        ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search action or shortcut"));
        if let Some(action) = self.recording {
            ui.label(format!(
                "Press a shortcut for {} (Escape cancels)",
                action.label()
            ));
            let chord = ui.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key {
                        key,
                        modifiers,
                        pressed: true,
                        ..
                    } => Some(crate::keymap::event_chord(*key, *modifiers, mac)),
                    _ => None,
                })
            });
            if let Some(chord) = chord {
                if chord.key != "Escape" {
                    map.set_chords(action, vec![chord]);
                }
                self.recording = None;
            }
        }
        let conflicts = map.conflicts(mac);
        if !conflicts.is_empty() {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "Conflicting shortcuts: remove or rebind one action before saving.",
            );
        }
        let query = self.query.to_lowercase();
        for action in ActionId::ALL {
            let label = map
                .chords_for(*action)
                .iter()
                .map(|c| c.label(mac))
                .collect::<Vec<_>>()
                .join(", ");
            if !format!("{} {} {}", action.label(), action.id(), label)
                .to_lowercase()
                .contains(&query)
            {
                continue;
            }
            ui.push_id(action.id(), |ui| {
                ui.horizontal(|ui| {
                    ui.label(action.label());
                    ui.label(label);
                    if ui.button("Record").clicked() {
                        self.recording = Some(*action);
                    }
                    if ui.small_button("Remove").clicked() {
                        map.unbind(*action);
                    }
                    if ui.small_button("Reset").clicked() {
                        let base = Keymap::preset(preset).unwrap_or_default();
                        map.set_chords(
                            *action,
                            base.chords_for(*action).into_iter().cloned().collect(),
                        );
                    }
                });
            });
        }
    }
}
