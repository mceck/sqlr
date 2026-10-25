//! The Help > Keyboard Shortcuts window.

use egui::Context;
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
        let response = theme::Dialog::new(
            "keyboard_shortcuts",
            theme::icons::KEYBOARD,
            "Keyboard shortcuts",
        )
        .subtitle("Every command and its keys")
        .width(580.0)
        .body_height(560.0)
        .show(ctx, |d| {
            d.body(|ui| action = self.view.show(ui, keymap, preset));
            d.footer(|f| f.cancel_as("Close"));
        });
        self.open = !response.closed && action.is_none();
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
