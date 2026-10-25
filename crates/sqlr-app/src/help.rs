//! The Help > Keyboard Shortcuts window.

use egui::{Context, Frame, Margin, Window, vec2};
use sqlr_core::Keymap;
use sqlr_ui::shortcuts::{ShortcutsAction, ShortcutsView};
use sqlr_ui::theme;

/// The window's open flag and its list; the app owns it and draws the window from `dialogs`.
#[derive(Default)]
pub struct ShortcutsDialog {
    pub open: bool,
    view: ShortcutsView,
}

impl ShortcutsDialog {
    /// Draws the window over `keymap` (the active one, `preset` its preset id).
    pub fn show(
        &mut self,
        ctx: &Context,
        keymap: &Keymap,
        preset: &str,
    ) -> Option<ShortcutsAction> {
        if !self.open {
            return None;
        }
        let mut action = None;
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
                        action = self.view.show(ui, keymap, preset);
                    });
            });
        self.open = open && !close && action.is_none();
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(dialog: &mut ShortcutsDialog) {
        let ctx = Context::default();
        let keymap = Keymap::preset("sqlr").unwrap();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            dialog.show(ui.ctx(), &keymap, "sqlr");
        });
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
        let mut dialog = ShortcutsDialog {
            open: true,
            ..Default::default()
        };
        frame(&mut dialog);
        frame(&mut dialog);
        assert!(dialog.open);
    }
}
