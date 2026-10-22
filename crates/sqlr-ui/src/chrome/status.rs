//! One segment of the status bar.

use egui::{Color32, Label, RichText};

use crate::theme;

/// `text` on one line, in `color` (the muted text color by default), cut with an ellipsis
/// when the bar is too narrow.
pub fn status_segment(ui: &mut egui::Ui, text: &str, color: Option<Color32>) -> egui::Response {
    let color = color.unwrap_or_else(|| theme::palette(ui.ctx()).text_muted);
    ui.add(Label::new(RichText::new(text).color(color)).truncate())
}
