//! A centered placeholder for a pane with nothing to show.

use egui::RichText;

use crate::theme::{self, Spacing};

/// `title` over a muted `hint`, centered, with an optional accent button labeled `action`.
/// Returns `true` when that button is clicked.
pub fn empty_state(ui: &mut egui::Ui, title: &str, hint: &str, action: Option<&str>) -> bool {
    let muted = theme::palette(ui.ctx()).text_muted;
    let mut clicked = false;
    ui.vertical_centered(|ui| {
        ui.add_space(Spacing::XL);
        ui.label(RichText::new(title).font(theme::semibold(ui.ctx(), 15.0)));
        ui.add_space(Spacing::S);
        ui.label(RichText::new(hint).color(muted));
        if let Some(action) = action {
            ui.add_space(Spacing::M);
            clicked = ui.add(theme::primary_button(ui.ctx(), action)).clicked();
        }
    });
    clicked
}
