//! The full-width list row shared by the explorer tree, the palettes, the snippets and history
//! lists and the Drivers list: hover and selection cover the whole row, not just its label.

use egui::{CornerRadius, Id, InnerResponse, Layout, Popup, Rect, Sense, Shape, Ui, vec2};

use super::Spacing;

/// A row of the given `height` spanning the available width. Its response is registered before
/// `add_contents`, so buttons inside the row (an expand arrow, row actions) keep their own
/// clicks while everything else (labels, icons, empty space) clicks, double-clicks, drags and
/// right-clicks the row. Draw the row's name with a plain, non-selectable label: the row
/// paints the hover and selection fill behind it, and keeps the hover fill while its
/// [`super::row_context_menu`] is open.
pub fn list_row<R>(
    ui: &mut Ui,
    id: Id,
    selected: bool,
    height: f32,
    sense: Sense,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    let p = super::palette(ui.ctx());
    let rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), height));
    let response = ui.interact(rect, id, sense);
    let background = ui.painter().add(Shape::Noop);
    let inner = ui
        .allocate_ui_with_layout(
            rect.size(),
            Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_size(rect.size());
                add_contents(ui)
            },
        )
        .inner;
    let menu_open = Popup::is_id_open(ui.ctx(), Popup::default_response_id(&response));
    let hovered =
        response.contains_pointer() && !response.ctx.dragged_id().is_some_and(|d| d != id);
    let fill = if selected {
        Some(p.selection)
    } else if menu_open || hovered {
        Some(p.hover)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter().set(
            background,
            Shape::rect_filled(rect, CornerRadius::same(Spacing::RADIUS_S), fill),
        );
    }
    InnerResponse::new(inner, response)
}
