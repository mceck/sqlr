//! The visual design: light and dark palettes, Inter, Phosphor icons and the spacing scale.
//! Widgets read colors through [`palette`] and the code font through [`monospace`]; the app calls [`install_fonts`] once and [`apply`] each
//! frame.

mod apply;
pub mod fonts;
pub mod icons;
mod tokens;

pub use apply::{apply, dock_style, monospace, palette};
pub use fonts::{install_fonts, semibold};
pub use sqlr_core::ThemePref;
pub use tokens::{EditorColors, Palette, Spacing, contrast_ratio};

use egui::{
    Align2, CornerRadius, FontId, Layout, Margin, Response, RichText, Sense, Stroke, TextStyle,
    WidgetInfo, WidgetType, vec2,
};

/// A context menu styled like the rest of the app: roomier than egui's compact default and a
/// touch smaller than body text. Submenus inherit the same `MenuConfig`.
pub fn context_menu<R>(
    response: &Response,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<egui::InnerResponse<R>> {
    let p = palette(&response.ctx);
    let style = egui::style::StyleModifier::new(move |style| {
        egui::containers::menu::menu_style(style);
        let s = &mut style.spacing;
        s.button_padding = vec2(5.0, 4.0);
        s.item_spacing = vec2(14.0, 2.0);
        s.menu_margin = Margin::same(4);
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(12.0));
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(12.0));
        // Menu entries read softer at rest and brighten under the pointer.
        let v = &mut style.visuals;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text_muted);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.active.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.open.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.hovered.weak_bg_fill = p.hover;
        v.widgets.active.weak_bg_fill = p.hover;
    });
    let config = egui::containers::menu::MenuConfig::new().style(style.clone());
    egui::Popup::context_menu(response)
        .style(style)
        .info(
            egui::UiStackInfo::new(egui::UiKind::Menu)
                .with_tag_value(egui::containers::menu::MenuConfig::MENU_CONFIG_TAG, config),
        )
        .show(add_contents)
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

/// Shared chrome for the operational tools: centered, bounded, with a clear heading and
/// scrollable contents on small displays. Closing retains each caller's cleanup semantics.
pub fn tool_dialog<R>(
    ctx: &egui::Context,
    open: &mut bool,
    title: &str,
    icon: &str,
    description: &str,
    width: f32,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    let screen = ctx.content_rect();
    let width = width.min((screen.width() - 32.0).max(240.0));
    let mut close = false;
    let response = egui::Window::new(format!("{title} dialog"))
        .id(egui::Id::new(title))
        .open(open)
        .title_bar(false)
        .collapsible(false)
        .resizable(true)
        .default_width(width)
        .default_height((screen.height() - 160.0).max(160.0))
        .min_width(320.0_f32.min(width))
        .default_pos(egui::pos2(
            screen.center().x - width / 2.0,
            screen.top() + (screen.height() * 0.12).min(96.0),
        ))
        .constrain_to(screen.shrink(8.0))
        .frame(egui::Frame::window(&ctx.global_style()).inner_margin(Margin::ZERO))
        .show(ctx, |ui| {
            close = dialog_header(ui, icon, title, Some(description));
            egui::Frame::new()
                .inner_margin(Margin::same(16))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt((title, "body"))
                        .max_height((screen.height() - 180.0).max(100.0))
                        .auto_shrink([false, true])
                        .show(ui, body)
                        .inner
                })
                .inner
        });
    if close {
        *open = false;
    }
    response.and_then(|response| response.inner)
}

/// The default height of a query editor above its results: about a third of the tab, so the
/// result grid gets the larger share. Bounded so both panes stay usable on any window.
pub fn editor_split_height(available: f32) -> f32 {
    (available * 0.3).clamp(100.0, 220.0)
}

/// The custom title bar shared by the app's dialogs: an accent badge with `icon`, `title`,
/// an optional subtitle and a close button. It paints the window's rounded top corners and
/// closes with a hairline, so dialogs use a frame with no inner margin and no native title
/// bar. Returns `true` when the close button was clicked.
pub fn dialog_header(ui: &mut egui::Ui, icon: &str, title: &str, subtitle: Option<&str>) -> bool {
    let p = palette(ui.ctx());
    let mut close = false;
    egui::Frame::new()
        .fill(p.bg_panel)
        .corner_radius(CornerRadius {
            nw: Spacing::RADIUS_M,
            ne: Spacing::RADIUS_M,
            sw: 0,
            se: 0,
        })
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = Spacing::M;
                let (rect, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                ui.painter()
                    .circle_filled(rect.center(), 16.0, p.accent_soft);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    icon,
                    icons::font(ui, 17.0),
                    p.accent,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new(title).font(semibold(ui.ctx(), 15.0)));
                    if let Some(subtitle) = subtitle {
                        ui.label(RichText::new(subtitle).small().color(p.text_muted));
                    }
                });
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = format!("Close {title}");
                    if icons::icon_button(ui, icons::CLOSE, Some(p.text_muted), &label).clicked() {
                        close = true;
                    }
                });
            });
        });
    hairline(ui);
    close
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
                editor < available - editor,
                "editor {editor} of {available}: the grid must get the larger share"
            );
        }
    }
}
