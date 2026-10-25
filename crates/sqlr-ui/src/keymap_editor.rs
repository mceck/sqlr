//! Search, record and reset action bindings.
use egui::{
    Align, Color32, CornerRadius, Layout, Rect, RichText, Sense, Stroke, StrokeKind, UiBuilder,
    WidgetInfo, WidgetType, vec2,
};
use sqlr_core::{ActionId, Keymap, PRESETS};

use crate::theme::{self, Spacing, icons};

const ROW: f32 = 34.0;
const CAP_HEIGHT: f32 = 22.0;
/// The room the Remove and Reset buttons keep at the end of a row, shown or not.
const ACTIONS_WIDTH: f32 = 2.0 * 20.0 + Spacing::S;

/// The VS Code preset's id.
pub const VSCODE: &str = "vscode";

/// "Using VS Code? <link>" in muted text; true when the link was clicked.
pub fn vscode_hint(ui: &mut egui::Ui, link: &str) -> bool {
    let p = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        icons::paint(ui, icons::KEYBOARD, p.text_muted);
        ui.label(RichText::new("Using VS Code?").color(p.text_muted));
        ui.link(link).clicked()
    })
    .inner
}

#[derive(Default)]
pub struct KeymapEditor {
    query: String,
    recording: Option<ActionId>,
}

/// The keys of a chord label as separate caps: `⇧⌘↵` is three, `Ctrl+Shift+Enter` too,
/// `⌘Esc` two (the modifier glyphs, then the key whole).
fn caps(label: &str) -> Vec<String> {
    if label.chars().count() > 1 && label.contains('+') {
        let mut parts: Vec<String> = label.split('+').map(str::to_string).collect();
        // A trailing "+" key leaves an empty last part.
        if parts.len() >= 2 && parts[parts.len() - 1].is_empty() {
            parts.pop();
            if let Some(last) = parts.last_mut() {
                *last = "+".into();
            }
        }
        parts.retain(|p| !p.is_empty());
        parts
    } else {
        let key = label.trim_start_matches(['⌃', '⌥', '⇧', '⌘']);
        let mut out: Vec<String> = label[..label.len() - key.len()]
            .chars()
            .map(String::from)
            .collect();
        if !key.is_empty() {
            out.push(key.to_string());
        }
        out
    }
}

/// How a binding pill looks.
#[derive(Clone, Copy, PartialEq)]
enum PillState {
    Normal,
    Conflict,
    Recording,
}

/// The shortcut of one action, drawn as key caps. Clicking it records a new one. `label` is
/// the accessible name.
fn pill(ui: &mut egui::Ui, chords: &[String], state: PillState, label: &str) -> egui::Response {
    let p = theme::palette(ui.ctx());
    let unset = chords.is_empty() && state != PillState::Recording;
    let groups: Vec<Vec<String>> = match state {
        PillState::Recording => vec![vec!["Press a shortcut…".to_string()]],
        _ if chords.is_empty() => vec![vec!["Not set".to_string()]],
        _ => chords.iter().map(|c| caps(c)).collect(),
    };
    let (text_color, border, fill) = match state {
        PillState::Recording => (p.accent, p.accent, p.accent_soft),
        PillState::Conflict => (p.error, p.error, Color32::TRANSPARENT),
        PillState::Normal if unset => (p.text_disabled, p.border, Color32::TRANSPARENT),
        PillState::Normal => (p.text, p.border_input, p.bg_subtle),
    };
    let hover_border = (state == PillState::Normal).then_some(p.accent);
    let response = paint_caps(
        ui,
        &groups,
        Sense::click(),
        (text_color, border, fill),
        hover_border,
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    response.on_hover_text("Click to record a new shortcut")
}

/// Read-only key caps for the chords of one action (`⇧⌘Z`, `⌘Y`); a dim "—" when there are
/// none. The accessible name is the chords joined by " or ", or "Not set".
pub fn keycaps(ui: &mut egui::Ui, chords: &[String]) -> egui::Response {
    let p = theme::palette(ui.ctx());
    if chords.is_empty() {
        let response =
            ui.add(egui::Label::new(RichText::new("—").color(p.text_disabled)).selectable(false));
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, "Not set"));
        return response;
    }
    // One cap per chord, as macOS menus write them: lighter than a cap per key.
    let groups: Vec<Vec<String>> = chords.iter().map(|c| vec![c.clone()]).collect();
    let response = paint_caps(
        ui,
        &groups,
        Sense::hover(),
        (p.text, p.border_input, p.bg_subtle),
        None,
    );
    let name = chords.join(" or ");
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &name));
    response
}

/// Lays out and paints groups of caps (one group per chord) in `colors` (text, border,
/// fill); the border turns `hover_border` while hovered.
fn paint_caps(
    ui: &mut egui::Ui,
    groups: &[Vec<String>],
    sense: Sense,
    (text_color, border, fill): (Color32, Color32, Color32),
    hover_border: Option<Color32>,
) -> egui::Response {
    let font = egui::FontId::proportional(12.0);
    // Lay the caps out first to know the width.
    let galleys: Vec<Vec<_>> = groups
        .iter()
        .map(|g| {
            g.iter()
                .map(|k| {
                    ui.painter()
                        .layout_no_wrap(k.clone(), font.clone(), text_color)
                })
                .collect()
        })
        .collect();
    let cap_width = |w: f32| (w + 10.0).max(CAP_HEIGHT);
    let gap = 3.0;
    let mut width = 0.0;
    for (n, g) in galleys.iter().enumerate() {
        if n > 0 {
            width += 8.0;
        }
        width += g.iter().map(|k| cap_width(k.size().x)).sum::<f32>()
            + gap * g.len().saturating_sub(1) as f32;
    }
    let (rect, response) = ui.allocate_exact_size(vec2(width, CAP_HEIGHT), sense);
    let border = match hover_border {
        Some(color) if response.hovered() => color,
        _ => border,
    };
    let mut x = rect.left();
    for (n, g) in galleys.into_iter().enumerate() {
        if n > 0 {
            x += 8.0;
        }
        for k in g {
            let w = cap_width(k.size().x);
            let cap = Rect::from_min_size(egui::pos2(x, rect.top()), vec2(w, CAP_HEIGHT));
            let radius = CornerRadius::same(Spacing::RADIUS_S);
            ui.painter().rect_filled(cap, radius, fill);
            ui.painter()
                .rect_stroke(cap, radius, Stroke::new(1.0, border), StrokeKind::Inside);
            ui.painter()
                .galley(cap.center() - k.size() / 2.0, k, text_color);
            x += w + gap;
        }
    }
    response
}

impl KeymapEditor {
    pub fn show(&mut self, ui: &mut egui::Ui, map: &mut Keymap, preset: &mut String) {
        let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
        let p = theme::palette(ui.ctx());

        // Preset, reset and search on one line.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::M;
            ui.label(RichText::new("Preset").color(p.text_muted));
            egui::ComboBox::from_id_salt("keymap_preset")
                .selected_text(
                    PRESETS
                        .iter()
                        .find(|(id, _)| id == preset)
                        .map_or(preset.as_str(), |(_, label)| *label),
                )
                .show_ui(ui, |ui| {
                    for (id, label) in PRESETS {
                        if ui.selectable_label(preset == id, *label).clicked() {
                            *preset = id.to_string();
                            *map = Keymap::preset(id).unwrap_or_default();
                        }
                    }
                });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Reset all").clicked() {
                    *map = Keymap::preset(preset).unwrap_or_default();
                    self.recording = None;
                }
                crate::text_menu::text_edit(
                    ui,
                    egui::TextEdit::singleline(&mut self.query)
                        .desired_width(f32::INFINITY)
                        .hint_text("Search action or shortcut"),
                );
            });
        });
        if preset.as_str() != VSCODE {
            ui.add_space(Spacing::S);
            if vscode_hint(ui, "Switch to the VS Code preset") {
                *preset = VSCODE.to_string();
                *map = Keymap::preset(VSCODE).unwrap_or_default();
                self.recording = None;
            }
        }
        ui.add_space(Spacing::M);

        if let Some(action) = self.recording {
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
            egui::Frame::new()
                .fill(Color32::from_rgba_unmultiplied(
                    p.error.r(),
                    p.error.g(),
                    p.error.b(),
                    28,
                ))
                .corner_radius(Spacing::RADIUS_M)
                .inner_margin(egui::Margin::symmetric(Spacing::L as i8, Spacing::M as i8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        icons::paint(ui, icons::WARNING, p.error);
                        ui.colored_label(
                            p.error,
                            "Conflicting shortcuts: remove or rebind one action before saving.",
                        );
                    });
                });
            ui.add_space(Spacing::M);
        }
        let in_conflict: Vec<ActionId> = conflicts.into_iter().flat_map(|(_, a)| a).collect();

        let query = self.query.to_lowercase();
        let mut group = "";
        let base = Keymap::preset(preset).unwrap_or_default();
        for action in ActionId::ALL {
            let labels: Vec<String> = map
                .chords_for(*action)
                .iter()
                .map(|c| c.label(mac))
                .collect();
            let text = labels.join(", ");
            if !format!("{} {} {}", action.label(), action.id(), text)
                .to_lowercase()
                .contains(&query)
            {
                continue;
            }
            if action.group() != group {
                group = action.group();
                ui.add_space(Spacing::M);
                ui.label(
                    RichText::new(group.to_uppercase())
                        .small()
                        .strong()
                        .color(p.text_muted),
                );
                ui.add_space(Spacing::XS);
            }
            ui.push_id(action.id(), |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
                let recording = self.recording == Some(*action);
                let hovered = ui.rect_contains_pointer(rect);
                if recording || hovered {
                    let fill = if recording {
                        p.accent_soft
                    } else {
                        p.bg_subtle
                    };
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(Spacing::RADIUS_M), fill);
                }
                ui.painter().hline(
                    rect.x_range(),
                    rect.bottom() - 0.5,
                    Stroke::new(1.0, p.border),
                );
                let inner = rect.shrink2(vec2(Spacing::M, 0.0));
                // Name on the left.
                let mut left = ui.new_child(
                    UiBuilder::new()
                        .max_rect(inner)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                left.label(action.label());
                // Buttons, then the shortcut, right-aligned.
                let mut right = ui.new_child(
                    UiBuilder::new()
                        .max_rect(inner)
                        .layout(Layout::right_to_left(Align::Center)),
                );
                right.spacing_mut().item_spacing.x = Spacing::S;
                let differs = map.chords_for(*action) != base.chords_for(*action);
                let bound = !labels.is_empty();
                right.allocate_ui_with_layout(
                    vec2(ACTIONS_WIDTH, ROW),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        if bound && icons::row_button(ui, icons::CLOSE, "Remove").clicked() {
                            map.unbind(*action);
                        }
                        if differs && icons::row_button(ui, icons::UNDO, "Reset").clicked() {
                            map.set_chords(
                                *action,
                                base.chords_for(*action).into_iter().cloned().collect(),
                            );
                        }
                    },
                );
                let state = if recording {
                    PillState::Recording
                } else if in_conflict.contains(action) {
                    PillState::Conflict
                } else {
                    PillState::Normal
                };
                let label = format!("Record shortcut for {}", action.label());
                if pill(&mut right, &labels, state, &label).clicked() {
                    self.recording = Some(*action);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::caps;

    #[test]
    fn caps_split_modifiers_from_the_key() {
        assert_eq!(caps("⇧⌘↵"), ["⇧", "⌘", "↵"]);
        assert_eq!(caps("Esc"), ["Esc"]);
        assert_eq!(caps("⌘Esc"), ["⌘", "Esc"]);
        assert_eq!(caps("⌃Space"), ["⌃", "Space"]);
        assert_eq!(caps("F2"), ["F2"]);
        assert_eq!(caps("Ctrl+Shift+Enter"), ["Ctrl", "Shift", "Enter"]);
        assert_eq!(caps("Ctrl++"), ["Ctrl", "+"]);
    }
}
