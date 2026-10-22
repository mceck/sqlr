//! The toolbar's connection chip: the active tab's connection, and a popup to switch it or
//! act on it.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, FontId, Popup, RichText, ScrollArea, Sense, Stroke, WidgetInfo,
    WidgetType, vec2,
};

use crate::theme::{self, Palette, Spacing, icons};

/// A connection as the chip lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct ChipItem {
    pub id: String,
    pub name: String,
    /// Muted text after the name: the driver's short name.
    pub detail: String,
    /// The driver id, for its icon.
    pub driver: String,
    /// The connection's (or its profile's) color.
    pub color: Option<Color32>,
    pub state: ChipState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipState {
    Offline,
    Connecting,
    Connected,
    Failed,
}

impl ChipState {
    fn color(self, p: &Palette) -> Color32 {
        match self {
            Self::Offline => p.text_disabled,
            Self::Connecting => p.warning,
            Self::Connected => p.success,
            Self::Failed => p.error,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Offline => "Offline",
            Self::Connecting => "Connecting…",
            Self::Connected => "Connected",
            Self::Failed => "Connection failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChipAction {
    /// Point the tab at this connection id.
    Select(String),
    New,
    /// Edit, reconnect or disconnect the current connection.
    Edit,
    Reconnect,
    Disconnect,
}

/// The chip's widest and narrowest sizes; it shrinks with the space it is given.
const MAX_WIDTH: f32 = 220.0;
const MIN_WIDTH: f32 = 72.0;
const POPUP_WIDTH: f32 = 260.0;
const POPUP_LIST_HEIGHT: f32 = 320.0;

/// The chip of `current` (or "No connection"): state dot, driver icon, name, driver short
/// name and a chevron, tinted with the connection's color. A click opens a popup listing
/// the connections `items` returns (called only while the popup is open), then New
/// connection…, Edit connection…, Reconnect and Disconnect. Its accessible name is the
/// connection's name.
pub fn connection_chip(
    ui: &mut egui::Ui,
    current: Option<&ChipItem>,
    items: impl FnOnce() -> Vec<ChipItem>,
) -> Option<ChipAction> {
    let p = theme::palette(ui.ctx());
    let name = current.map_or("No connection", |c| c.name.as_str());
    let height = icons::TOOLBAR_SIZE.y;
    let width = ui.available_width().clamp(MIN_WIDTH, MAX_WIDTH);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::ComboBox, true, name));

    if ui.is_rect_visible(rect) {
        let fill = match current.and_then(|c| c.color) {
            Some(c) => {
                let alpha = if response.hovered() { 80 } else { 56 };
                Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
            }
            None if response.hovered() => p.hover,
            None => p.bg_subtle,
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(Spacing::RADIUS_M), fill);
        let mut x = rect.left() + Spacing::M;
        if let Some(c) = current {
            painter.circle_filled(egui::pos2(x + 3.0, rect.center().y), 3.5, c.state.color(&p));
            x += 6.0 + Spacing::S;
            let icon =
                egui::Rect::from_min_size(egui::pos2(x, rect.center().y - 8.0), vec2(16.0, 16.0));
            icons::paint_database_at(ui, icon, &c.driver);
            x += 16.0 + Spacing::S;
        }
        let chevron_x = rect.right() - Spacing::M - 6.0;
        painter.text(
            egui::pos2(chevron_x, rect.center().y),
            Align2::CENTER_CENTER,
            icons::CARET_DOWN,
            icons::font(ui, 11.0),
            p.text_muted,
        );
        let mut job = LayoutJob::default();
        let color = if current.is_some() {
            p.text
        } else {
            p.text_muted
        };
        job.append(
            name,
            0.0,
            TextFormat::simple(FontId::proportional(13.0), color),
        );
        if let Some(c) = current.filter(|c| !c.detail.is_empty()) {
            job.append(
                &c.detail,
                Spacing::S + 2.0,
                TextFormat::simple(FontId::proportional(12.0), p.text_muted),
            );
        }
        job.wrap = TextWrapping::truncate_at_width((chevron_x - 10.0 - x).max(0.0));
        let galley = ui.painter().layout_job(job);
        let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, color);
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                CornerRadius::same(Spacing::RADIUS_S),
                Stroke::new(1.0, p.accent),
                egui::StrokeKind::Inside,
            );
        }
    }
    let response = match current {
        Some(c) => {
            response.on_hover_text(format!("{} · {} · {}", c.name, c.detail, c.state.word()))
        }
        None => response.on_hover_text("Choose the tab's connection"),
    };

    let mut action = None;
    Popup::menu(&response)
        .width(POPUP_WIDTH.max(width))
        .show(|ui| {
            ui.label(RichText::new("CONNECTIONS").small().color(p.text_muted));
            ScrollArea::vertical()
                .max_height(POPUP_LIST_HEIGHT)
                .show(ui, |ui| {
                    for item in &items() {
                        let selected = current.is_some_and(|c| c.id == item.id);
                        if chip_row(ui, &p, item, selected).clicked() {
                            action = Some(ChipAction::Select(item.id.clone()));
                        }
                    }
                });
            ui.separator();
            if icons::menu_item(ui, icons::ADD, "New connection…").clicked() {
                action = Some(ChipAction::New);
            }
            let some = current.is_some();
            let live = current.is_some_and(|c| c.state != ChipState::Offline);
            if icons::menu_item_enabled(ui, some, icons::EDIT, "Edit connection…").clicked() {
                action = Some(ChipAction::Edit);
            }
            if icons::menu_item_enabled(ui, some, icons::REFRESH, "Reconnect").clicked() {
                action = Some(ChipAction::Reconnect);
            }
            if icons::menu_item_enabled(ui, live, icons::CLOSE, "Disconnect").clicked() {
                action = Some(ChipAction::Disconnect);
            }
        });
    action
}

/// One connection in the popup: state dot, driver icon, name (cut with an ellipsis, full in
/// the tooltip) and the driver's short name. Its accessible name is the connection's name.
fn chip_row(ui: &mut egui::Ui, p: &Palette, item: &ChipItem, selected: bool) -> egui::Response {
    let height = Spacing::ROW_TREE;
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, &item.name));
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            p.selection
        } else if response.hovered() {
            p.hover
        } else {
            Color32::TRANSPARENT
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(Spacing::RADIUS_S), fill);
        let mut x = rect.left() + Spacing::S;
        painter.circle_filled(
            egui::pos2(x + 3.0, rect.center().y),
            3.5,
            item.state.color(p),
        );
        x += 6.0 + Spacing::S;
        let icon =
            egui::Rect::from_min_size(egui::pos2(x, rect.center().y - 8.0), vec2(16.0, 16.0));
        icons::paint_database_at(ui, icon, &item.driver);
        x += 16.0 + Spacing::S;
        let detail = ui.painter().layout_no_wrap(
            item.detail.clone(),
            FontId::proportional(12.0),
            p.text_muted,
        );
        let detail_x = rect.right() - Spacing::S - detail.size().x;
        let mut job =
            LayoutJob::simple_singleline(item.name.clone(), FontId::proportional(13.0), p.text);
        job.wrap = TextWrapping::truncate_at_width((detail_x - Spacing::M - x).max(0.0));
        let name = ui.painter().layout_job(job);
        let y = rect.center().y;
        ui.painter()
            .galley(egui::pos2(x, y - name.size().y / 2.0), name, p.text);
        ui.painter().galley(
            egui::pos2(detail_x, y - detail.size().y / 2.0),
            detail,
            p.text_muted,
        );
    }
    response.on_hover_text(format!("{} · {}", item.name, item.state.word()))
}
