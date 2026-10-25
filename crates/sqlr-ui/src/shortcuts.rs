//! The Help > Keyboard Shortcuts list: every action of the active keymap (user changes and
//! preset included), grouped and searchable, with its keys as caps in the platform's
//! notation. Unbound actions show a dim "—".

use egui::{Align, Layout, RichText, Sense, TextEdit, vec2};
use sqlr_core::{ACTION_GROUPS, ActionId, Keymap};

use crate::keymap_editor::{VSCODE, keycaps, vscode_hint};
use crate::theme::{self, Spacing};

const ROW: f32 = 30.0;

/// What the user asked the list for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutsAction {
    /// Open Preferences → Keymap.
    OpenKeymap,
}

#[derive(Default)]
pub struct ShortcutsView {
    pub query: String,
}

/// Whether `action` matches a lower-cased `query` by name, group or keys.
fn matches(action: ActionId, keys: &[String], query: &str) -> bool {
    query.is_empty()
        || action.label().to_lowercase().contains(query)
        || action.group().to_lowercase().contains(query)
        || keys.iter().any(|k| k.to_lowercase().contains(query))
}

impl ShortcutsView {
    /// Draws the search row and the grouped list; `preset` is the keymap's preset id.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        keymap: &Keymap,
        preset: &str,
    ) -> Option<ShortcutsAction> {
        let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
        let p = theme::palette(ui.ctx());
        let mut action = None;
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.link("Customize in Keymap…").clicked() {
                    action = Some(ShortcutsAction::OpenKeymap);
                }
                ui.add_space(Spacing::M);
                crate::text_menu::text_edit(
                    ui,
                    TextEdit::singleline(&mut self.query)
                        .id_salt("shortcuts_search")
                        .hint_text("Search commands or keys")
                        .desired_width(f32::INFINITY),
                );
            });
        });
        if preset != VSCODE {
            ui.add_space(Spacing::S);
            if vscode_hint(ui, "Switch the keymap preset…") {
                action = Some(ShortcutsAction::OpenKeymap);
            }
        }
        ui.add_space(Spacing::S);

        let query = self.query.trim().to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("shortcuts_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut any = false;
                for group in ACTION_GROUPS {
                    let rows: Vec<(ActionId, Vec<String>)> = ActionId::ALL
                        .iter()
                        .filter(|a| a.group() == *group)
                        .map(|a| (*a, keymap.labels(*a, mac)))
                        .filter(|(a, keys)| matches(*a, keys, &query))
                        .collect();
                    if rows.is_empty() {
                        continue;
                    }
                    any = true;
                    ui.add_space(Spacing::M);
                    ui.label(
                        RichText::new(group.to_uppercase())
                            .small()
                            .strong()
                            .color(p.text_muted),
                    );
                    ui.add_space(Spacing::XS);
                    for (action, keys) in rows {
                        ui.push_id(action.id(), |ui| {
                            let (rect, _) = ui.allocate_exact_size(
                                vec2(ui.available_width(), ROW),
                                Sense::hover(),
                            );
                            ui.painter().hline(
                                rect.x_range(),
                                rect.bottom() - 0.5,
                                egui::Stroke::new(1.0, p.border),
                            );
                            // Clear of the overlaid scroll bar on the right.
                            let mut inner = rect.shrink2(vec2(Spacing::S, 0.0));
                            inner.max.x -= Spacing::M;
                            let mut row = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(inner)
                                    .layout(Layout::right_to_left(Align::Center)),
                            );
                            keycaps(&mut row, &keys);
                            row.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let color = if keys.is_empty() {
                                    p.text_muted
                                } else {
                                    p.text
                                };
                                ui.add(
                                    egui::Label::new(RichText::new(action.label()).color(color))
                                        .truncate(),
                                );
                            });
                        });
                    }
                }
                if !any {
                    ui.add_space(Spacing::L);
                    ui.label(
                        RichText::new(format!("No shortcut matches “{}”", self.query.trim()))
                            .color(p.text_muted),
                    );
                }
                ui.add_space(Spacing::L);
                theme::hint(
                    ui,
                    "Text fields always use the standard keys (arrows, Backspace, Delete). \
                     Enter confirms a dialog and Esc dismisses it.",
                );
            });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use sqlr_core::Chord;

    struct State {
        view: ShortcutsView,
        keymap: Keymap,
        preset: String,
        actions: Vec<ShortcutsAction>,
    }

    fn harness(keymap: Keymap, preset: &str) -> Harness<'static, State> {
        let state = State {
            view: ShortcutsView::default(),
            keymap,
            preset: preset.into(),
            actions: Vec::new(),
        };
        let mut h = Harness::builder()
            .with_size(vec2(560.0, 2400.0))
            .with_os(egui::os::OperatingSystem::Mac)
            .build_ui_state(
                |ui, s: &mut State| {
                    if let Some(a) = s.view.show(ui, &s.keymap, &s.preset) {
                        s.actions.push(a);
                    }
                },
                state,
            );
        h.run_steps(2);
        h
    }

    #[test]
    fn lists_actions_by_group_with_platform_keys() {
        let h = harness(Keymap::preset("sqlr").unwrap(), "sqlr");
        // One heading per group, in order.
        let mut last = f32::MIN;
        for group in ACTION_GROUPS {
            let top = h.get_by_label(&group.to_uppercase()).rect().top();
            assert!(top > last, "{group} out of order");
            last = top;
        }
        // Keys in macOS notation, commit now bound.
        h.get_by_label("⌘↵");
        h.get_by_label("⌥⌘↵");
        h.get_by_label("⇧⌘Z or ⌘Y");
        // An action of the Query group sits between the Query and Editor headings.
        let run = h.get_by_label("Run statement or selection").rect().top();
        assert!(run > h.get_by_label("QUERY").rect().top());
        assert!(run < h.get_by_label("EDITOR").rect().top());
    }

    #[test]
    fn unbound_actions_show_a_dash() {
        let mut keymap = Keymap::preset("sqlr").unwrap();
        keymap.unbind(ActionId::Commit);
        keymap.set_chords(ActionId::Explain, vec![Chord::parse("F7").unwrap()]);
        let h = harness(keymap, "sqlr");
        assert!(h.query_by_label("⌥⌘↵").is_none());
        h.get_by_label("F7");
        // Unbound actions (Commit and the ones without a default) read "Not set" to
        // assistive tech and draw a dim dash.
        assert!(h.get_all_by_label("Not set").count() >= 2);
    }

    #[test]
    fn search_filters_by_name_and_keys() {
        let mut h = harness(Keymap::preset("sqlr").unwrap(), "sqlr");
        h.state_mut().view.query = "explain".into();
        h.run_steps(2);
        h.get_by_label("Explain");
        h.get_by_label("Explain analyze (runs the statement)");
        assert!(h.query_by_label("Run script").is_none());
        assert!(h.query_by_label("EDITOR").is_none());
        // By keys.
        h.state_mut().view.query = "⇧⌘↵".into();
        h.run_steps(2);
        h.get_by_label("Run script");
        assert!(h.query_by_label("Explain").is_none());
        // Nothing.
        h.state_mut().view.query = "zzz".into();
        h.run_steps(2);
        h.get_by_label("No shortcut matches “zzz”");
    }

    #[test]
    fn links_open_the_keymap_and_the_vscode_hint_follows_the_preset() {
        let mut h = harness(Keymap::preset("sqlr").unwrap(), "sqlr");
        h.get_by_label("Customize in Keymap…").click();
        h.run_steps(2);
        h.get_by_label("Switch the keymap preset…").click();
        h.run_steps(2);
        assert_eq!(
            h.state().actions,
            [ShortcutsAction::OpenKeymap, ShortcutsAction::OpenKeymap]
        );
        let h = harness(Keymap::preset("vscode").unwrap(), "vscode");
        assert!(h.query_by_label("Switch the keymap preset…").is_none());
        h.get_by_label("⌥⇧F");
    }
}
