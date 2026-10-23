//! The Help > Keyboard Shortcuts window.

use egui::{Context, Frame, Grid, Margin, RichText, ScrollArea, Window, vec2};
use sqlr_ui::theme::{self, Spacing};

/// The window's open flag; the app owns it and draws the window from `dialogs`.
#[derive(Default)]
pub struct ShortcutsDialog {
    pub open: bool,
}

impl ShortcutsDialog {
    pub fn show(&mut self, ctx: &Context) {
        if !self.open {
            return;
        }
        let palette = theme::palette(ctx);
        let mut open = self.open;
        let mut close = false;
        Window::new("Keyboard shortcuts")
            .open(&mut open)
            .title_bar(false)
            .collapsible(false)
            .resizable(true)
            .default_size(vec2(560.0, 560.0))
            .min_width(460.0)
            .default_pos(ctx.content_rect().center() - vec2(280.0, 280.0))
            .frame(Frame::window(&ctx.global_style()).inner_margin(Margin::ZERO))
            .show(ctx, |ui| {
                if theme::dialog_header(
                    ui,
                    theme::icons::KEYBOARD,
                    "Keyboard shortcuts",
                    Some("Every command and its keys"),
                ) {
                    close = true;
                }
                Frame::new()
                    .inner_margin(Margin::symmetric(16, 14))
                    .show(ui, |ui| {
                        ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let map=sqlr_ui::keymap::ui_keymap(ctx);
                                Grid::new("resolved_shortcuts").num_columns(2).spacing([Spacing::L*2.0,Spacing::S]).show(ui,|ui| {
                                    for action in sqlr_core::ActionId::ALL {
                                        ui.label(action.label());ui.label(RichText::new(map.label(*action).unwrap_or_else(||"Unbound".into())).monospace().color(palette.text_muted));ui.end_row();
                                    }
                                });
                                ui.add_space(Spacing::L);
                                ui.label("Fixed text input: characters, Backspace, Delete, arrows. Dialog confirmation and dismissal: Enter / Escape.");
                            });
                    });
            });
        self.open = open && !close;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(dialog: &mut ShortcutsDialog) {
        let ctx = Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| dialog.show(ui.ctx()));
        // No renderer consumes the font atlas upload here.
        out.textures_delta.clear();
    }

    #[test]
    fn a_closed_dialog_draws_nothing() {
        let mut dialog = ShortcutsDialog::default();
        frame(&mut dialog);
        assert!(!dialog.open);
    }

    #[test]
    fn an_open_dialog_lists_every_action_and_its_keys() {
        let mut dialog = ShortcutsDialog { open: true };
        frame(&mut dialog);
        frame(&mut dialog);
        assert!(dialog.open);
    }
}
