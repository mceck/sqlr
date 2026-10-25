//! The visual design: light and dark palettes, Inter, Phosphor icons and the spacing scale.
//! Widgets read colors through [`palette`] and the code font through [`monospace`]; the app calls [`install_fonts`] once and [`apply`] each
//! frame.

mod apply;
mod dialog;
pub mod fonts;
pub mod icons;
pub mod menu;
mod row;
mod tokens;
mod tooltip;

pub use apply::{apply, dock_style, monospace, palette};
pub use dialog::{Dialog, DialogFooter, DialogResponse, DialogUi, confirm, scroll_body};
pub use fonts::{install_fonts, semibold};
pub use row::list_row;
pub use sqlr_core::ThemePref;
pub use tokens::{EditorColors, Palette, Spacing, contrast_ratio};
pub use tooltip::{tooltip, tooltip_for};

use egui::{
    FontId, Margin, Response, RichText, Sense, Stroke, TextStyle, WidgetInfo, WidgetType, vec2,
};

/// The style of every menu (context menus, the menu bar's menus and their submenus): roomier
/// than egui's compact default, body-sized text, entries softer at rest that brighten on a
/// subtle hover fill.
pub fn menu_style(ctx: &egui::Context) -> egui::style::StyleModifier {
    let p = palette(ctx);
    egui::style::StyleModifier::new(move |style| {
        egui::containers::menu::menu_style(style);
        let s = &mut style.spacing;
        s.button_padding = vec2(6.0, 4.0);
        s.item_spacing = vec2(14.0, 2.0);
        s.menu_margin = Margin::same(4);
        s.interact_size.y = 26.0;
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(13.0));
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(13.0));
        let v = &mut style.visuals;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text_muted);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.active.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.open.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.hovered.weak_bg_fill = p.hover;
        v.widgets.active.weak_bg_fill = p.hover;
        v.widgets.open.weak_bg_fill = p.hover;
    })
}

/// The `MenuConfig` carrying [`menu_style`], for menus opened from a menu bar or a button.
pub fn menu_config(ctx: &egui::Context) -> egui::containers::menu::MenuConfig {
    egui::containers::menu::MenuConfig::new().style(menu_style(ctx))
}

/// A context menu in the app's [`menu_style`], opened at the pointer. Submenus inherit the
/// same `MenuConfig`. Build its entries with [`menu`].
pub fn context_menu<R>(
    response: &Response,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<egui::InnerResponse<R>> {
    show_menu(egui::Popup::context_menu(response), response, add_contents)
}

/// [`context_menu`] for a list or tree row: it opens at the pointer's x just below the row,
/// so the row it acts on (name, state, actions) stays visible. Rows drawn with [`list_row`]
/// stay highlighted while it is open.
pub fn row_context_menu<R>(
    response: &Response,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<egui::InnerResponse<R>> {
    let anchor = egui::Popup::default_response_id(response).with("sqlr_row_anchor");
    if response.secondary_clicked() {
        let x = response
            .interact_pointer_pos()
            .map_or(response.rect.left(), |p| p.x);
        let pos = egui::pos2(x, response.rect.bottom());
        response.ctx.data_mut(|d| d.insert_temp(anchor, pos));
    }
    let pos = response
        .ctx
        .data(|d| d.get_temp::<egui::Pos2>(anchor))
        .unwrap_or(response.rect.left_bottom());
    let popup = egui::Popup::context_menu(response)
        .anchor(egui::PopupAnchor::Position(pos))
        .align(egui::RectAlign::BOTTOM_START);
    show_menu(popup, response, add_contents)
}

fn show_menu<R>(
    popup: egui::Popup<'_>,
    response: &Response,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<egui::InnerResponse<R>> {
    let style = menu_style(&response.ctx);
    let config = egui::containers::menu::MenuConfig::new().style(style.clone());
    popup
        .style(style)
        .info(
            egui::UiStackInfo::new(egui::UiKind::Menu)
                .with_tag_value(egui::containers::menu::MenuConfig::MENU_CONFIG_TAG, config),
        )
        .show(|ui| {
            ui.set_min_width(menu::MIN_WIDTH);
            add_contents(ui)
        })
}

/// A much smaller monospace font (grid column types, compact details).
pub(crate) fn small_monospace(ctx: &egui::Context) -> FontId {
    let mut font = monospace(ctx);
    font.size = (font.size - 4.0).max(8.5);
    font
}

/// The palette for `pref`; `System` follows the OS (`ctx.system_theme()`), dark when unknown.
pub fn palette_for(ctx: &egui::Context, pref: ThemePref) -> Palette {
    match pref {
        ThemePref::Dark => Palette::dark(),
        ThemePref::Light => Palette::light(),
        ThemePref::System => match ctx.system_theme() {
            Some(egui::Theme::Light) => Palette::light(),
            _ => Palette::dark(),
        },
    }
}

/// A dialog's primary action: text on an `accent` fill. Its label stays `label`.
pub fn primary_button(ctx: &egui::Context, label: &str) -> egui::Button<'static> {
    let p = palette(ctx);
    egui::Button::new(egui::RichText::new(label.to_string()).color(p.accent_text)).fill(p.accent)
}

/// [`primary_button`] when `enabled`, otherwise the same label on a quiet disabled fill.
/// The caller still wraps it in `add_enabled`: this only keeps the accent from suggesting
/// that a disabled action is available.
pub fn primary_button_state(
    ctx: &egui::Context,
    enabled: bool,
    label: &str,
) -> egui::Button<'static> {
    if enabled {
        return primary_button(ctx, label);
    }
    let p = palette(ctx);
    egui::Button::new(egui::RichText::new(label.to_string()).color(p.text_disabled))
        .fill(p.bg_subtle)
}

/// A field caption, shown above its field in dialogs.
pub fn field_label(ui: &mut egui::Ui, text: &str) {
    let muted = palette(ui.ctx()).text_muted;
    ui.add_space(Spacing::XS);
    ui.label(egui::RichText::new(text).color(muted));
}

/// A section title inside a dialog or a panel.
pub fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(semibold(ui.ctx(), 13.0)));
}

/// A useful empty state for a side panel, with an explanation of its next action.
pub fn empty_panel(ui: &mut egui::Ui, icon: &str, title: &str, description: &str) {
    let p = palette(ui.ctx());
    egui::Frame::new()
        .inner_margin(Margin::symmetric(12, 20))
        .show(ui, |ui| {
            icons::paint(ui, icon, p.text_muted);
            ui.add_space(Spacing::M);
            section_title(ui, title);
            ui.add_space(Spacing::S);
            ui.label(RichText::new(description).color(p.text_muted));
        });
}

/// The default height of a query editor above its results: about a fifth of the tab, so the
/// result grid gets the larger share. Bounded so both panes stay usable on any window.
pub fn editor_split_height(available: f32) -> f32 {
    (available * 0.2).clamp(80.0, 180.0)
}

/// A muted, smaller line: hints, descriptions, empty states.
pub fn hint(ui: &mut egui::Ui, text: &str) {
    let muted = palette(ui.ctx()).text_muted;
    ui.label(RichText::new(text).small().color(muted));
}

/// The label column of a settings/data-source grid. Unlike [`field_label`], it sits to the
/// left of its field and uses the normal text color.
pub fn grid_label(ui: &mut egui::Ui, text: &str) -> Response {
    ui.label(text)
}

/// An underlined dialog tab (General, Options, SSH/SSL…). `label` is its accessible name.
pub fn dialog_tab(ui: &mut egui::Ui, active: bool, label: &str) -> Response {
    let p = palette(ui.ctx());
    let font = if active {
        semibold(ui.ctx(), 13.0)
    } else {
        FontId::proportional(13.0)
    };
    let color = if active { p.text } else { p.text_muted };
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, color);
    let width = galley.size().x + 2.0 * Spacing::L;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 32.0), Sense::click());
    if response.hovered() {
        let fill = if active { p.hover } else { p.bg_subtle };
        ui.painter()
            .rect_filled(rect.shrink2(vec2(0.0, 3.0)), 4, fill);
    }
    let text_pos = egui::pos2(
        rect.center().x - galley.size().x / 2.0,
        rect.center().y - galley.size().y / 2.0,
    );
    ui.painter().galley(text_pos, galley, color);
    if active {
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 1.0,
            Stroke::new(2.0, p.accent),
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(2.0),
            4,
            Stroke::new(1.0, p.accent),
            egui::StrokeKind::Inside,
        );
    }
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, active, label));
    response
}

/// A compact toggle for a segmented control (theme choice, fetch presets).
pub fn segment(ui: &mut egui::Ui, active: bool, label: &str) -> Response {
    let p = palette(ui.ctx());
    let selected = active;
    let button = egui::Button::new(RichText::new(label).color(if selected {
        p.accent_text
    } else {
        p.text
    }))
    .fill(if selected { p.accent } else { p.bg_subtle })
    .stroke(if selected {
        Stroke::NONE
    } else {
        Stroke::new(1.0, p.border_input)
    });
    let response = ui.add(button);
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, label));
    response
}

/// A 1 px line in the panel color, spanning the available width.
pub fn hairline(ui: &mut egui::Ui) {
    let p = palette(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, p.border));
}

#[cfg(test)]
mod tests {
    use super::editor_split_height;

    #[test]
    fn the_editor_starts_smaller_than_the_result_grid() {
        for available in [400.0, 700.0, 1000.0] {
            let editor = editor_split_height(available);
            assert!(
                editor <= available * 0.25,
                "editor {editor} of {available}: the grid must get the larger share"
            );
            assert!(
                editor < available - editor,
                "editor {editor} of {available}: the grid must get the larger share"
            );
        }
    }
}
