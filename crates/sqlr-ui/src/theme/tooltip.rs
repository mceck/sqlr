//! Tooltips for icon buttons: the command's name with its shortcut in the weak text color,
//! and an optional one-line explanation below. Shown on disabled buttons too, so a dimmed
//! icon still says what it is.

use egui::{Response, RichText};
use sqlr_core::ActionId;

use super::Spacing;

/// The widest a tooltip's explanation grows before it wraps.
const MAX_WIDTH: f32 = 300.0;

/// Attaches the tooltip `title` · `shortcut` (· `detail` on its own line) to `response`.
pub fn tooltip(
    response: Response,
    title: &str,
    shortcut: Option<&str>,
    detail: Option<&str>,
) -> Response {
    let add = |ui: &mut egui::Ui| {
        let p = super::palette(ui.ctx());
        ui.set_max_width(MAX_WIDTH);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::M;
            ui.label(RichText::new(title).color(p.text));
            if let Some(keys) = shortcut {
                ui.label(RichText::new(keys).color(p.text_muted));
            }
        });
        if let Some(detail) = detail {
            ui.label(RichText::new(detail).small().color(p.text_muted));
        }
    };
    response.on_hover_ui(add).on_disabled_hover_ui(add)
}

/// [`tooltip`] with `action`'s current shortcut from the keymap.
pub fn tooltip_for(
    response: Response,
    title: &str,
    action: ActionId,
    detail: Option<&str>,
) -> Response {
    let shortcut = crate::keymap::shortcut(&response.ctx, action);
    tooltip(response, title, shortcut.as_deref(), detail)
}
