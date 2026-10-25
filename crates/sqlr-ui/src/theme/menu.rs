//! Menu entries shared by the context menus and the in-window menu bar: every entry reserves
//! the same icon gutter, so labels line up whether or not an entry has an icon; shortcut
//! hints sit right-aligned in the weak text color; destructive entries use the error color.
//! Group entries with [`separator`].

use egui::containers::menu::{MenuButton, SubMenuButton};
use egui::{
    Atom, Button, Color32, InnerResponse, Popup, Response, RichText, Stroke, Ui, WidgetInfo,
    WidgetType, vec2,
};

use super::icons;

/// The width reserved before every label for its icon (or for none).
pub const GUTTER: f32 = icons::SIZE + 4.0;

/// The least space between a label and its shortcut hint.
const SHORTCUT_GAP: f32 = 24.0;

/// The narrowest a menu gets, so short menus do not look cramped.
pub const MIN_WIDTH: f32 = 180.0;

/// One menu entry. Build it with [`MenuItem::new`] and show it with [`MenuItem::ui`], or use
/// the shorthands [`item`], [`danger_item`], [`check_item`] and [`submenu`].
#[derive(Clone, Debug)]
pub struct MenuItem<'a> {
    icon: Option<&'a str>,
    label: &'a str,
    shortcut: Option<String>,
    danger: bool,
    enabled: bool,
    checked: Option<bool>,
}

impl<'a> MenuItem<'a> {
    /// An enabled entry labeled `label` (also its accessible name), with no icon.
    pub fn new(label: &'a str) -> Self {
        Self {
            icon: None,
            label,
            shortcut: None,
            danger: false,
            enabled: true,
            checked: None,
        }
    }

    /// The glyph drawn in the gutter.
    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The keyboard shortcut shown on the right, e.g. `⌘C`.
    pub fn shortcut(mut self, shortcut: Option<impl Into<String>>) -> Self {
        self.shortcut = shortcut.map(Into::into);
        self
    }

    /// A destructive entry (Delete, Drop): label and icon in the error color.
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// A choice: a check mark in the gutter while `checked`, reported as selected.
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    fn button(&self, ui: &Ui) -> Button<'static> {
        let p = super::palette(ui.ctx());
        // A disabled entry is faded as a whole by `add_enabled`.
        let tone = if self.danger && self.enabled {
            p.error
        } else if self.checked == Some(true) && self.enabled {
            p.accent
        } else {
            p.text_muted
        };
        let glyph = match (self.checked, self.icon) {
            (Some(true), _) => Some(icons::CHECK),
            (Some(false), _) => None,
            (None, icon) => icon,
        };
        let size = vec2(GUTTER, icons::SIZE);
        let gutter = match glyph {
            Some(glyph) => {
                let mut atom: Atom<'static> =
                    icons::text(ui, glyph, icons::SIZE).color(tone).into();
                atom.size = Some(size);
                atom
            }
            None => Atom {
                size: Some(size),
                ..Default::default()
            },
        };
        let mut label = RichText::new(self.label.to_string());
        if self.danger && self.enabled {
            label = label.color(p.error);
        }
        let mut button = Button::new((gutter, label));
        if let Some(shortcut) = &self.shortcut {
            // A fixed gap keeps the hint clear of the label in a narrow menu.
            let gap = Atom {
                size: Some(vec2(SHORTCUT_GAP, 0.0)),
                ..Default::default()
            };
            button = button.shortcut_text((gap, shortcut.clone()));
        }
        button
    }

    fn info(&self) -> WidgetInfo {
        match self.checked {
            Some(checked) => {
                WidgetInfo::selected(WidgetType::Button, self.enabled, checked, self.label)
            }
            None => WidgetInfo::labeled(WidgetType::Button, self.enabled, self.label),
        }
    }

    pub fn ui(self, ui: &mut Ui) -> Response {
        let button = self.button(ui);
        let response = ui.add_enabled(self.enabled, button);
        let info = self.info();
        response.widget_info(|| info.clone());
        response
    }
}

/// A menu entry with `icon`, `label` and an optional right-aligned `shortcut`.
pub fn item(ui: &mut Ui, icon: &str, label: &str, shortcut: Option<&str>) -> Response {
    MenuItem::new(label).icon(icon).shortcut(shortcut).ui(ui)
}

/// A destructive entry (Delete, Drop…) in the error color. Put a [`separator`] before it.
pub fn danger_item(ui: &mut Ui, icon: &str, label: &str) -> Response {
    MenuItem::new(label).icon(icon).danger().ui(ui)
}

/// One choice of a group (a radio-like submenu): a check mark in the gutter when `checked`.
pub fn check_item(ui: &mut Ui, checked: bool, label: &str) -> Response {
    MenuItem::new(label).checked(checked).ui(ui)
}

/// A submenu entry: `icon` (or an empty gutter), `label` and egui's arrow. Its accessible
/// name is `"{label} ⏵"`.
pub fn submenu<R>(
    ui: &mut Ui,
    icon: Option<&str>,
    label: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<Option<R>> {
    let mut entry = MenuItem::new(label);
    if let Some(icon) = icon {
        entry = entry.icon(icon);
    }
    let button = entry.button(ui).right_text(SubMenuButton::RIGHT_ARROW);
    let (response, inner) = SubMenuButton::from_button(button).ui(ui, |ui| {
        ui.set_min_width(MIN_WIDTH * 0.75);
        add_contents(ui)
    });
    let accessible = format!("{label} {}", SubMenuButton::RIGHT_ARROW);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, accessible.clone()));
    InnerResponse::new(inner.map(|i| i.inner), response)
}

/// A hairline between groups of entries.
pub fn separator(ui: &mut Ui) {
    ui.separator();
}

/// A muted note under a menu's entries, aligned with their labels and wrapped to the menu's
/// width: where the related actions live, when they are not in this menu.
pub fn hint(ui: &mut Ui, text: &str) {
    let muted = super::palette(ui.ctx()).text_muted;
    ui.horizontal(|ui| {
        ui.add_space(GUTTER + ui.spacing().button_padding.x);
        ui.set_max_width(MIN_WIDTH + 80.0);
        ui.add(egui::Label::new(RichText::new(text).small().color(muted)).wrap());
    });
}

/// A title of the in-window menu bar (File, Edit…): flat text at rest, a subtle fill on hover
/// and while its menu is open. The menu uses the app's [`super::menu_style`]. Inside a menu
/// (the compact menu of a narrow window) it is a [`submenu`] instead.
pub fn bar_menu<R>(
    ui: &mut Ui,
    label: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<Option<R>> {
    if egui::containers::menu::is_in_menu(ui) {
        return submenu(ui, None, label, add_contents);
    }
    let p = super::palette(ui.ctx());
    let config = super::menu_config(ui.ctx());
    ui.scope(|ui| {
        let open = Popup::is_id_open(ui.ctx(), ui.next_auto_id().with("popup"));
        let w = &mut ui.style_mut().visuals.widgets;
        for v in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            v.bg_stroke = Stroke::NONE;
            v.fg_stroke = Stroke::new(1.0, p.text);
        }
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.hovered.weak_bg_fill = p.hover;
        w.active.weak_bg_fill = p.hover;
        w.open.weak_bg_fill = p.hover;
        if open {
            w.inactive = w.open;
        }
        let (response, inner) = MenuButton::new(label).config(config).ui(ui, |ui| {
            ui.set_min_width(MIN_WIDTH);
            add_contents(ui)
        });
        InnerResponse::new(inner.map(|i| i.inner), response)
    })
    .inner
}
