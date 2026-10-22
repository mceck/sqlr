//! The welcome screen, shown when no editor tab is open: the app mark, cards that start a
//! new connection, the demo database, and the recently used connections.

use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, FontId, Galley, Rect, RichText, Sense, Stroke, StrokeKind,
    WidgetInfo, WidgetType, pos2, vec2,
};

use crate::theme::{self, Spacing, icons};

/// A card that opens the new-connection dialog on a known database.
#[derive(Clone, Debug, PartialEq)]
pub struct DriverCard {
    /// The known database's id (`sqlr_core::known_database`).
    pub id: String,
    pub name: String,
    /// "TCP server", "File".
    pub detail: String,
    /// The driver id whose database mark the card shows.
    pub icon: &'static str,
}

/// A recently used connection.
#[derive(Clone, Debug, PartialEq)]
pub struct RecentItem {
    pub id: String,
    pub name: String,
    /// "PostgreSQL · db.example", "SQLite · app.db".
    pub detail: String,
    pub color: Option<Color32>,
    /// "5 min ago".
    pub when: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WelcomeAction {
    /// Open the new-connection dialog, preset for this known database when given.
    NewConnection(Option<String>),
    /// Open the catalog of known databases.
    MoreDatabases,
    OpenDemo,
    /// Connect this connection and open an editor tab on it.
    OpenRecent(String),
}

/// The widest the welcome column gets.
const COLUMN: f32 = 760.0;
const CARD_HEIGHT: f32 = 64.0;
const CARD_GAP: f32 = 10.0;
/// Cards narrower than this wrap onto another row.
const CARD_MIN: f32 = 150.0;
const RECENT_ROW: f32 = 30.0;

/// Shows the welcome screen; returns what the user picked.
pub fn welcome(
    ui: &mut egui::Ui,
    cards: &[DriverCard],
    recent: &[RecentItem],
) -> Option<WelcomeAction> {
    let palette = theme::palette(ui.ctx());
    let mut action = None;
    egui::ScrollArea::vertical()
        .id_salt("welcome")
        .auto_shrink(false)
        .show(ui, |ui| {
            let width = (ui.available_width() - 2.0 * Spacing::XL).clamp(120.0, COLUMN);
            let side = ((ui.available_width() - width) / 2.0).max(0.0);
            ui.add_space(48.0);
            ui.horizontal(|ui| {
                ui.add_space(side);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    header(ui, width);
                    ui.add_space(28.0);
                    section(ui, "CREATE A CONNECTION");
                    if let Some(a) = card_grid(ui, cards, width) {
                        action = Some(a);
                    }
                    ui.add_space(Spacing::XL);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = Spacing::M;
                        if ui
                            .add(theme::primary_button(ui.ctx(), "Open demo database"))
                            .clicked()
                        {
                            action = Some(WelcomeAction::OpenDemo);
                        }
                        if ui.button("New connection…").clicked() {
                            action = Some(WelcomeAction::NewConnection(None));
                        }
                    });
                    ui.add_space(28.0);
                    section(ui, "RECENT");
                    if recent.is_empty() {
                        ui.label(
                            RichText::new("No recent connections yet").color(palette.text_muted),
                        );
                    }
                    for item in recent {
                        if recent_row(ui, item, width) {
                            action = Some(WelcomeAction::OpenRecent(item.id.clone()));
                        }
                    }
                    ui.add_space(Spacing::XL);
                });
            });
        });
    action
}

/// The mark, the name and the tagline, centered on the column.
fn header(ui: &mut egui::Ui, width: f32) {
    let palette = theme::palette(ui.ctx());
    let (row, _) = ui.allocate_exact_size(vec2(width, 46.0), Sense::hover());
    let mark = Rect::from_center_size(row.center(), vec2(46.0, 46.0));
    ui.painter()
        .rect_filled(mark, CornerRadius::same(11), palette.accent);
    ui.painter().text(
        mark.center(),
        Align2::CENTER_CENTER,
        icons::DATA_SOURCE,
        icons::font(ui, 26.0),
        palette.accent_text,
    );
    ui.add_space(Spacing::L);
    ui.vertical_centered(|ui| {
        ui.label(
            RichText::new("sqlr")
                .font(theme::semibold(ui.ctx(), 27.0))
                .color(palette.text),
        );
        ui.label(RichText::new("A fast, native database client").color(palette.text_muted));
    });
}

fn section(ui: &mut egui::Ui, title: &str) {
    let palette = theme::palette(ui.ctx());
    ui.label(
        RichText::new(title)
            .font(theme::semibold(ui.ctx(), 10.5))
            .color(palette.text_muted),
    );
    ui.add_space(Spacing::M);
}

/// `text` on one line, cut with an ellipsis to `width`.
fn one_line(ui: &egui::Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = TextWrapping::truncate_at_width(width.max(1.0));
    ui.painter().layout_job(job)
}

/// The driver cards and "More databases…", as many per row as fit.
fn card_grid(ui: &mut egui::Ui, cards: &[DriverCard], width: f32) -> Option<WelcomeAction> {
    let more = DriverCard {
        id: String::new(),
        name: "More databases…".into(),
        detail: "Compatible and planned".into(),
        icon: "database",
    };
    let all: Vec<&DriverCard> = cards.iter().chain(std::iter::once(&more)).collect();
    let per_row = ((width + CARD_GAP) / (CARD_MIN + CARD_GAP))
        .floor()
        .clamp(1.0, all.len() as f32) as usize;
    let card_width = (width - CARD_GAP * (per_row as f32 - 1.0)) / per_row as f32;
    let mut action = None;
    for chunk in all.chunks(per_row) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = CARD_GAP;
            for card in chunk {
                if card_ui(ui, card, card_width) {
                    action = Some(if card.id.is_empty() {
                        WelcomeAction::MoreDatabases
                    } else {
                        WelcomeAction::NewConnection(Some(card.id.clone()))
                    });
                }
            }
        });
        ui.add_space(CARD_GAP);
    }
    action
}

/// One card: the database mark, the name and the detail. Its accessibility label is the name.
fn card_ui(ui: &mut egui::Ui, card: &DriverCard, width: f32) -> bool {
    let palette = theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(width, CARD_HEIGHT), Sense::click());
    let name = card.name.clone();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name.clone()));
    let fill = if response.hovered() {
        palette.hover
    } else {
        palette.bg_raised
    };
    let stroke = if response.hovered() {
        palette.accent
    } else {
        palette.border
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(Spacing::RADIUS_M),
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    let icon = Rect::from_min_size(
        pos2(rect.left() + Spacing::L, rect.center().y - 10.0),
        vec2(20.0, 20.0),
    );
    icons::paint_database_at(ui, icon, card.icon);
    let text_left = icon.right() + Spacing::M;
    let text_width = rect.right() - Spacing::M - text_left;
    let title = one_line(
        ui,
        &card.name,
        theme::semibold(ui.ctx(), 13.0),
        palette.text,
        text_width,
    );
    let detail = one_line(
        ui,
        &card.detail,
        FontId::proportional(11.5),
        palette.text_muted,
        text_width,
    );
    let top = rect.center().y - (title.size().y + detail.size().y + Spacing::XS) / 2.0;
    ui.painter()
        .galley(pos2(text_left, top), title.clone(), palette.text);
    ui.painter().galley(
        pos2(text_left, top + title.size().y + Spacing::XS),
        detail,
        palette.text_muted,
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// One recent connection: color dot, name, detail, and how long ago on the right. The
/// whole row is one button labeled with the name.
fn recent_row(ui: &mut egui::Ui, item: &RecentItem, width: f32) -> bool {
    let palette = theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(width, RECENT_ROW), Sense::click());
    let name = item.name.clone();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name.clone()));
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(Spacing::RADIUS_M), palette.hover);
    }
    let y = rect.center().y;
    let dot = pos2(rect.left() + Spacing::M + 4.0, y);
    match item.color {
        Some(color) => {
            ui.painter().circle_filled(dot, 4.0, color);
        }
        None => {
            ui.painter()
                .circle_stroke(dot, 3.5, Stroke::new(1.0, palette.text_disabled));
        }
    }
    let when = one_line(
        ui,
        &item.when,
        FontId::proportional(11.5),
        palette.text_muted,
        120.0,
    );
    let when_left = rect.right() - Spacing::M - when.size().x;
    ui.painter().galley(
        pos2(when_left, y - when.size().y / 2.0),
        when,
        palette.text_muted,
    );
    let left = dot.x + Spacing::L;
    let room = (when_left - Spacing::L - left).max(0.0);
    let title = one_line(
        ui,
        &item.name,
        FontId::proportional(13.0),
        palette.text,
        room * 0.55,
    );
    ui.painter().galley(
        pos2(left, y - title.size().y / 2.0),
        title.clone(),
        palette.text,
    );
    let detail_left = left + title.size().x + Spacing::M;
    let detail = one_line(
        ui,
        &item.detail,
        FontId::proportional(11.5),
        palette.text_muted,
        (when_left - Spacing::L - detail_left).max(0.0),
    );
    ui.painter().galley(
        pos2(detail_left, y - detail.size().y / 2.0),
        detail,
        palette.text_muted,
    );
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let response = if item.name.chars().count() > 40 {
        response.on_hover_text(&item.name)
    } else {
        response
    };
    response.clicked()
}
