//! Turns the tokens into egui's `Style` and egui_dock's `Style`, and keeps the active palette
//! in the context for widgets to read.

use egui::style::{ScrollStyle, Selection, WidgetVisuals, Widgets};
use egui::{
    Color32, CornerRadius, FontFamily, FontId, Id, Margin, Shadow, Stroke, TextStyle, Theme, vec2,
};

use super::fonts::{INTER_SEMIBOLD, has_family};
use super::{Palette, Spacing};

/// What was last applied to a context; stored in its data.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Applied {
    palette: Palette,
    mono_size: f32,
    /// The heading style names the SemiBold family (it was installed when applied).
    semibold: bool,
}

fn key() -> Id {
    Id::new("sqlr_theme_applied")
}

fn applied(ctx: &egui::Context) -> Option<Applied> {
    ctx.data(|d| d.get_temp::<Applied>(key()))
}

/// Sets egui's style and visuals from `palette` (Body and Button 13 Inter, Small 11, Heading
/// 15 Inter SemiBold, Monospace `mono_size`) and stores the palette for
/// [`palette`]. Cheap when nothing changed, so it may run every frame; call it inside a frame.
pub fn apply(ctx: &egui::Context, palette: &Palette, mono_size: f32) {
    let semibold = has_family(ctx, &FontFamily::Name(INTER_SEMIBOLD.into()));
    let wanted = Applied {
        palette: *palette,
        mono_size,
        semibold,
    };
    if applied(ctx) == Some(wanted) {
        return;
    }
    let theme = if palette.is_dark() {
        Theme::Dark
    } else {
        Theme::Light
    };
    let mut style = (*ctx.style_of(theme)).clone();
    style_from(&mut style, palette, mono_size, semibold);
    ctx.set_style_of(theme, style);
    ctx.set_theme(theme);
    ctx.data_mut(|d| d.insert_temp(key(), wanted));
}

/// The palette last applied to `ctx`; [`Palette::dark`] if none was.
pub fn palette(ctx: &egui::Context) -> Palette {
    applied(ctx).map_or_else(Palette::dark, |a| a.palette)
}

/// The editor's and the grid's font (the default monospace font at the configured size).
pub fn monospace(ctx: &egui::Context) -> FontId {
    FontId::monospace(applied(ctx).map_or(13.0, |a| a.mono_size))
}

fn radius(r: u8) -> CornerRadius {
    CornerRadius::same(r)
}

fn widget(bg: Color32, stroke: Stroke, fg: Color32) -> WidgetVisuals {
    WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: stroke,
        corner_radius: radius(Spacing::RADIUS_S),
        fg_stroke: Stroke::new(1.0, fg),
        // egui drops the frame of unframed-until-hover buttons and labels, but the
        // padding it keeps is computed as `button_padding + expansion - stroke`; with no
        // expansion the border width made hovered buttons 2 px wider. The expansion
        // compensates for the stroke in every state, so only the background changes.
        expansion: 1.0,
    }
}

fn style_from(style: &mut egui::Style, p: &Palette, mono_size: f32, semibold: bool) {
    let heading = if semibold {
        FontId::new(15.0, FontFamily::Name(INTER_SEMIBOLD.into()))
    } else {
        FontId::proportional(15.0)
    };
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(11.0)),
        (TextStyle::Body, FontId::proportional(13.0)),
        (TextStyle::Button, FontId::proportional(13.0)),
        (TextStyle::Heading, heading),
        (TextStyle::Monospace, FontId::monospace(mono_size)),
    ]
    .into();

    let s = &mut style.spacing;
    s.item_spacing = vec2(Spacing::M, 6.0);
    s.button_padding = vec2(Spacing::M, Spacing::S);
    s.interact_size.y = Spacing::ROW_TREE;
    s.window_margin = Margin::same(Spacing::L as i8);
    s.menu_margin = Margin::same(6);
    s.indent = Spacing::XL;
    // Floating bars that never take layout space, but stay visible as a thin, dimmed handle
    // whenever an area overflows, so clipped lists always show that they scroll.
    s.scroll = ScrollStyle {
        bar_width: Spacing::M,
        floating_width: 3.0,
        dormant_handle_opacity: 0.35,
        active_handle_opacity: 0.6,
        ..ScrollStyle::floating()
    };

    let dark = p.is_dark();
    let border = Stroke::new(1.0, p.border);
    let input_border = Stroke::new(1.0, p.border_input);
    let v = &mut style.visuals;
    v.dark_mode = dark;
    v.override_text_color = None;
    // Hint text in fields and shortcut hints in menus.
    v.weak_text_color = Some(p.placeholder);
    v.hyperlink_color = p.accent;
    v.faint_bg_color = p.zebra();
    v.extreme_bg_color = p.bg_input;
    v.text_edit_bg_color = Some(p.bg_input);
    v.code_bg_color = p.hover;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.error;
    v.panel_fill = p.bg_panel;
    v.window_fill = p.bg_raised;
    v.window_stroke = input_border;
    v.window_corner_radius = radius(Spacing::RADIUS_M);
    v.menu_corner_radius = radius(Spacing::RADIUS_M);
    // Light dialogs need a deeper shadow to lift off a white page.
    let shadow_alpha = if dark { 89 } else { 46 }; // ≈ 0.35 / 0.18
    let shadow = Shadow {
        offset: [0, 4],
        blur: 20,
        spread: 0,
        color: Color32::from_black_alpha(shadow_alpha),
    };
    v.window_shadow = shadow;
    v.popup_shadow = shadow;
    // egui draws a selected label's text and a focused text field's border with this one
    // stroke: the focus color is the accent tuned to read on the selection, so a focused
    // field gets an accent ring rather than a heavy text-colored border.
    v.selection = Selection {
        bg_fill: p.selection,
        stroke: Stroke::new(1.0, p.focus),
    };
    // Disabled controls fade less, so their outline and label stay legible on white.
    v.disabled_alpha = 0.6;
    v.text_cursor.stroke = Stroke::new(2.0, p.accent);
    v.widgets = Widgets {
        // Labels, separators and frames.
        noninteractive: WidgetVisuals {
            expansion: 0.0,
            ..widget(p.bg_panel, border, p.text)
        },
        // At rest: inputs on the sunken background, secondary buttons on the subtle one.
        // Inputs keep a visible border against the panel background.
        inactive: WidgetVisuals {
            weak_bg_fill: p.bg_subtle,
            ..widget(p.bg_input, input_border, p.text)
        },
        hovered: widget(p.hover, input_border, p.text),
        // Pressed: the accent ring.
        active: widget(p.selection, Stroke::new(1.0, p.accent), p.text),
        open: widget(p.hover, input_border, p.text),
    };
}

/// egui_dock's style for the query tabs: `TAB` high, the active tab on the editor background,
/// close buttons faint until hovered.
pub fn dock_style(ctx: &egui::Context) -> egui_dock::Style {
    let p = palette(ctx);
    let mut style = egui_dock::Style::from_egui(&ctx.global_style());
    style.main_surface_border_stroke = Stroke::NONE;
    style.tab_bar.height = Spacing::TAB;
    style.tab_bar.bg_fill = p.bg_panel;
    style.tab_bar.hline_color = p.border;
    style.tab_bar.corner_radius = CornerRadius::ZERO;
    style.separator.color_idle = p.border;
    style.separator.color_hovered = p.accent;
    style.separator.color_dragged = p.accent;
    let tab = |bg: Color32, text: Color32| egui_dock::TabInteractionStyle {
        outline_color: Color32::TRANSPARENT,
        corner_radius: CornerRadius::ZERO,
        bg_fill: bg,
        text_color: text,
    };
    style.tab.active = tab(p.bg_editor, p.text);
    style.tab.focused = tab(p.bg_editor, p.text);
    style.tab.active_with_kb_focus = tab(p.bg_editor, p.text);
    style.tab.focused_with_kb_focus = tab(p.bg_editor, p.text);
    style.tab.inactive = tab(p.bg_panel, p.text_muted);
    style.tab.inactive_with_kb_focus = tab(p.bg_panel, p.text_muted);
    style.tab.hovered = tab(p.hover, p.text);
    style.tab.tab_body.bg_fill = p.bg_editor;
    style.tab.tab_body.stroke = Stroke::NONE;
    style.tab.tab_body.inner_margin = Margin::ZERO;
    style.tab.tab_body.corner_radius = CornerRadius::ZERO;
    style.tab.hline_below_active_tab_name = false;
    style.tab.minimum_width = Some(96.0);
    style.buttons.close_tab_color = p.text_disabled;
    style.buttons.close_tab_active_color = p.text;
    style.buttons.close_tab_bg_fill = p.hover;
    style.buttons.add_tab_color = p.text_muted;
    style.buttons.add_tab_active_color = p.text;
    style.buttons.add_tab_bg_fill = p.hover;
    style.buttons.add_tab_border_color = Color32::TRANSPARENT;
    style.overlay.selection_color = p.accent.gamma_multiply(0.3);
    style
}
