//! The find bar (⌘F) and its replace row (⌥⌘F), drawn over the top right of the editor. It
//! only collects input; the editor runs the [`FindAction`]s on its model.

use egui::{Align2, Area, Frame, Id, Key, Order, Rect, TextEdit, pos2};

use crate::editor::find::FindOptions;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FindAction {
    Next,
    Previous,
    Replace,
    ReplaceAll,
    Close,
}

#[derive(Default)]
pub(crate) struct FindBar {
    pub(crate) open: bool,
    pub(crate) replace: bool,
    pub(crate) needle: String,
    pub(crate) replacement: String,
    pub(crate) opts: FindOptions,
    focus_needle: bool,
    /// One of the bar's fields had keyboard focus in the last frame.
    focused: bool,
}

impl FindBar {
    /// Opens the bar (with the replace row if `replace`), focusing the needle field; a
    /// one-line selection becomes the needle.
    pub(crate) fn open(&mut self, replace: bool, selection: &str) {
        self.open = true;
        self.replace = replace;
        if !selection.is_empty() && !selection.contains('\n') {
            self.needle = selection.to_string();
        }
        self.focus_needle = true;
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.focused = false;
    }

    /// Shows the bar at the top right of `over`; returns what the user asked for.
    pub(crate) fn show(&mut self, ctx: &egui::Context, id: Id, over: Rect) -> Vec<FindAction> {
        let mut actions = Vec::new();
        if !self.open {
            return actions;
        }
        let needle_id = id.with("needle");
        let replace_id = id.with("replacement");
        let had_focus = self.focused;
        let mut now_focused = false;
        Area::new(id)
            .order(Order::Foreground)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(pos2(over.right() - 16.0, over.top() + 4.0))
            .show(ctx, |ui| {
                Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let label = ui.label("Find");
                        let field = crate::text_menu::text_edit(
                            ui,
                            TextEdit::singleline(&mut self.needle)
                                .id(needle_id)
                                .desired_width(180.0)
                                .return_key(None),
                        );
                        let field = field.labelled_by(label.id);
                        if std::mem::take(&mut self.focus_needle) {
                            field.request_focus();
                        }
                        now_focused |= field.has_focus();
                        ui.checkbox(&mut self.opts.case_sensitive, "Match case");
                        ui.checkbox(&mut self.opts.whole_word, "Whole word");
                        if ui.button("Previous").clicked() {
                            actions.push(FindAction::Previous);
                        }
                        if ui.button("Next").clicked() {
                            actions.push(FindAction::Next);
                        }
                    });
                    if self.replace {
                        ui.horizontal(|ui| {
                            let label = ui.label("Replace with");
                            let field = crate::text_menu::text_edit(
                                ui,
                                TextEdit::singleline(&mut self.replacement)
                                    .id(replace_id)
                                    .desired_width(180.0)
                                    .return_key(None),
                            );
                            now_focused |= field.labelled_by(label.id).has_focus();
                            if ui.button("Replace").clicked() {
                                actions.push(FindAction::Replace);
                            }
                            if ui.button("Replace all").clicked() {
                                actions.push(FindAction::ReplaceAll);
                            }
                        });
                    }
                });
            });
        // Keys typed into the bar's fields. egui drops a field's focus on Esc before the
        // frame runs, so Esc counts when a field had focus in the last frame.
        let keymap = crate::keymap::ui_keymap(ctx);
        if now_focused || had_focus {
            ctx.input_mut(|i| {
                if i.consume_key(egui::Modifiers::SHIFT, Key::Enter)
                    || keymap.consume(i, sqlr_core::ActionId::FindPrevious)
                {
                    actions.push(FindAction::Previous);
                }
                if i.consume_key(egui::Modifiers::NONE, Key::Enter)
                    || keymap.consume(i, sqlr_core::ActionId::FindNext)
                {
                    actions.push(FindAction::Next);
                }
                if keymap.consume(i, sqlr_core::ActionId::Replace) {
                    self.replace = true;
                }
                if i.consume_key(egui::Modifiers::NONE, Key::Escape) {
                    actions.push(FindAction::Close);
                }
            });
        }
        self.focused = now_focused;
        actions
    }
}
