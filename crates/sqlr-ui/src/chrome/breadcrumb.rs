//! The path strip above an editor: `connection › database › tab`.

use egui::{Align, Button, Layout, RichText};

use crate::theme::{self, Spacing, icons};

/// One clickable segment of a [`breadcrumb`].
#[derive(Clone, Debug, PartialEq)]
pub struct Crumb {
    pub label: String,
    /// An icon glyph painted before the label.
    pub icon: Option<&'static str>,
}

/// The widest a single segment may grow before it is cut with an ellipsis.
const MAX_CRUMB: f32 = 220.0;

/// `crumbs` separated by `›`, with `right` in muted text at the right edge. Segments are
/// cut with an ellipsis when the strip is narrow. Returns the index of a clicked segment.
pub fn breadcrumb(ui: &mut egui::Ui, crumbs: &[Crumb], right: &str) -> Option<usize> {
    let p = theme::palette(ui.ctx());
    let mut clicked = None;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        ui.add(egui::Label::new(RichText::new(right).small().color(p.text_muted)).truncate());
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = Spacing::XS;
            ui.spacing_mut().button_padding = egui::vec2(Spacing::S, 1.0);
            for (i, crumb) in crumbs.iter().enumerate() {
                if ui.available_width() < 24.0 {
                    break;
                }
                if i > 0 {
                    ui.label(RichText::new("›").color(p.text_disabled));
                }
                if let Some(icon) = crumb.icon {
                    icons::paint(ui, icon, p.text_muted);
                }
                let last = i + 1 == crumbs.len();
                let color = if last { p.text } else { p.text_muted };
                let width = ui.available_width().min(MAX_CRUMB);
                let response = ui
                    .scope(|ui| {
                        ui.set_max_width(width);
                        ui.add(
                            Button::new(RichText::new(&crumb.label).color(color))
                                .frame_when_inactive(false)
                                .truncate(),
                        )
                    })
                    .inner;
                if response.clicked() {
                    clicked = Some(i);
                }
            }
        });
    });
    clicked
}
