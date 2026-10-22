//! The toolbar's icon button.

use crate::theme::icons;

/// A 28×26 flat toolbar button showing `icon`, with a hover fill. `label` is its accessible
/// name and its tooltip.
pub fn tool_button(ui: &mut egui::Ui, icon: &str, label: &str, enabled: bool) -> egui::Response {
    icons::toolbar_button(ui, enabled, icon, None, false, label).on_hover_text(label)
}
