//! The welcome screen, shown when no editor tab is open: the app mark, cards that start a
//! new connection, the demo database, and the recently used connections.

use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Color32, CornerRadius, FontId, Galley, Rect, RichText, Sense, Stroke, StrokeKind, WidgetInfo,
    WidgetType, pos2, vec2,
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
    /// Open connection migration from another client or backup.
    ImportConnections,
    OpenDemo,
    /// Connect this connection and open an editor tab on it.
    OpenRecent(String),
}

/// The widest the welcome column gets.
const COLUMN: f32 = 940.0;
const CARD_HEIGHT: f32 = 96.0;
const CARD_GAP: f32 = 10.0;
/// Cards narrower than this wrap onto another row.
const CARD_MIN: f32 = 114.0;
const RECENT_ROW: f32 = 56.0;

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
            let available = ui.available_width();
            let margin = if available < 600.0 { 16.0 } else { 36.0 };
            let width = (available - 2.0 * margin).clamp(1.0, COLUMN);
            let side = ((available - width) / 2.0).max(0.0);
            ui.add_space(if available < 600.0 { 24.0 } else { 48.0 });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(side);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.spacing_mut().item_spacing.x = Spacing::M;
                    header(ui, width);
                    ui.add_space(32.0);
                    if connection_header(ui, width) {
                        action = Some(WelcomeAction::NewConnection(None));
                    }
                    ui.add_space(16.0);
                    if let Some(a) = card_grid(ui, cards, width) {
                        action = Some(a);
                    }
                    ui.add_space(6.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.link("More databases…").clicked() {
                            action = Some(WelcomeAction::MoreDatabases);
                        }
                    });
                    ui.add_space(24.0);
                    if let Some(a) = quick_start(ui, width) {
                        action = Some(a);
                    }
                    ui.add_space(28.0);
                    section(ui, "Recent connections", "Pick up where you left off.");
                    ui.add_space(12.0);
                    egui::Frame::new()
                        .fill(palette.bg_raised)
                        .stroke(Stroke::new(1.0, palette.border))
                        .corner_radius(12)
                        .inner_margin(8)
                        .show(ui, |ui| {
                            ui.set_width((width - 18.0).max(1.0));
                            if recent.is_empty() {
                                ui.add_space(12.0);
                                ui.vertical_centered(|ui| {
                                    ui.label(
                                        RichText::new("No recent connections yet")
                                            .color(palette.text_muted),
                                    );
                                    ui.label(
                                        RichText::new(
                                            "Your connections will appear here after you connect.",
                                        )
                                        .size(11.5)
                                        .color(palette.text_muted),
                                    );
                                });
                                ui.add_space(12.0);
                            }
                            for item in recent {
                                if recent_row(ui, item, ui.available_width()) {
                                    action = Some(WelcomeAction::OpenRecent(item.id.clone()));
                                }
                            }
                        });
                    ui.add_space(32.0);
                });
            });
        });
    action
}

fn connection_header(ui: &mut egui::Ui, width: f32) -> bool {
    let button = theme::primary_button(ui.ctx(), "New connection…");
    if width < 480.0 {
        section(
            ui,
            "Create a connection",
            "Choose a database to get started.",
        );
        ui.add_space(8.0);
        ui.add_sized(vec2(136.0, 32.0), button).clicked()
    } else {
        ui.horizontal(|ui| {
            section(
                ui,
                "Create a connection",
                "Choose a database to get started.",
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_sized(vec2(136.0, 32.0), button).clicked()
            })
            .inner
        })
        .inner
    }
}

/// The palettes' shortcuts, from the keymap: everything else is a search away.
fn key_hints(ui: &mut egui::Ui) {
    let palette = theme::palette(ui.ctx());
    ui.spacing_mut().item_spacing.y = Spacing::XS;
    for (action, label) in [
        (sqlr_core::ActionId::Commands, "Commands"),
        (sqlr_core::ActionId::OpenObject, "Go to a table"),
    ] {
        let Some(keys) = crate::keymap::shortcut(ui.ctx(), action) else {
            continue;
        };
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(label).size(12.0).color(palette.text_muted));
            crate::keymap_editor::keycaps(ui, &[keys]);
        });
    }
}

/// A compact brand header that leaves the connection choices in focus.
fn header(ui: &mut egui::Ui, width: f32) {
    let palette = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        let (mark, _) = ui.allocate_exact_size(vec2(60.0, 60.0), Sense::hover());
        ui.painter().image(
            app_icon_texture(ui.ctx()),
            mark,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.label(
                RichText::new("sqlr")
                    .font(theme::semibold(ui.ctx(), 32.0))
                    .color(palette.text),
            );
            ui.label(
                RichText::new("A fast, native database client")
                    .size(13.0)
                    .color(palette.text_muted),
            );
        });
        if width >= 480.0 {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.vertical(key_hints);
            });
        }
    });
    ui.add_space(20.0);
    let (line, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
    ui.painter().hline(
        line.x_range(),
        line.center().y,
        Stroke::new(1.0, palette.border),
    );
}

/// Two quiet entry points for exploring sqlr and bringing over an existing workspace.
fn quick_start(ui: &mut egui::Ui, width: f32) -> Option<WelcomeAction> {
    let palette = theme::palette(ui.ctx());
    let mut action = None;
    let columns = if width >= 600.0 { 2 } else { 1 };
    let card_width = (width - 12.0 * (columns - 1) as f32) / columns as f32;
    let mut tile = |ui: &mut egui::Ui, demo: bool| {
        egui::Frame::new()
            .fill(if demo {
                palette.bg_editor.lerp_to_gamma(palette.accent, 0.06)
            } else {
                palette.bg_subtle
            })
            .stroke(Stroke::new(
                1.0,
                if demo {
                    palette.bg_editor.lerp_to_gamma(palette.accent, 0.22)
                } else {
                    palette.border
                },
            ))
            .corner_radius(12)
            .inner_margin(18)
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width((card_width - 38.0).max(1.0));
                    ui.label(
                        RichText::new(if demo {
                            "Just exploring?"
                        } else {
                            "Make yourself at home"
                        })
                        .font(theme::semibold(ui.ctx(), 14.0))
                        .color(palette.text),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(if demo {
                            "Try sqlr with a ready-to-query sample database."
                        } else {
                            "Bring your saved connections from another client."
                        })
                        .size(12.0)
                        .color(palette.text_muted),
                    );
                    ui.add_space(10.0);
                    if demo {
                        if ui.button("Open demo database").clicked() {
                            action = Some(WelcomeAction::OpenDemo);
                        }
                    } else if ui.button("Import connections…").clicked() {
                        action = Some(WelcomeAction::ImportConnections);
                    }
                });
            });
    };
    if columns == 2 {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            tile(ui, true);
            tile(ui, false);
        });
    } else {
        tile(ui, true);
        ui.add_space(12.0);
        tile(ui, false);
    }
    action
}

fn app_icon_texture(ctx: &egui::Context) -> egui::TextureId {
    let id = egui::Id::new("sqlr-app-icon");
    if let Some(handle) = ctx.data(|data| data.get_temp::<egui::TextureHandle>(id)) {
        return handle.id();
    }

    let image = image::load_from_memory(include_bytes!("../../../docs/media/icon.png"))
        .expect("the bundled sqlr app icon is a valid image")
        .resize(256, 256, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let image = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    let handle = ctx.load_texture("sqlr-app-icon", image, egui::TextureOptions::LINEAR);
    let texture = handle.id();
    ctx.data_mut(|data| data.insert_temp(id, handle));
    texture
}

fn section(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    let palette = theme::palette(ui.ctx());
    ui.vertical(|ui| {
        ui.label(
            RichText::new(title)
                .font(theme::semibold(ui.ctx(), 19.0))
                .color(palette.text),
        );
        ui.label(RichText::new(subtitle).size(12.0).color(palette.text_muted));
    });
}

/// `text` on one line, cut with an ellipsis to `width`.
fn one_line(ui: &egui::Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = TextWrapping::truncate_at_width(width.max(1.0));
    ui.painter().layout_job(job)
}

/// Two balanced rows on desktop; wrap to more rows when the window gets narrow.
fn card_grid(ui: &mut egui::Ui, cards: &[DriverCard], width: f32) -> Option<WelcomeAction> {
    let target = cards.len().div_ceil(2).clamp(1, 4);
    let per_row = (((width + CARD_GAP) / (CARD_MIN + CARD_GAP)).floor() as usize).clamp(1, target);
    let card_width = (width - CARD_GAP * (per_row as f32 - 1.0)) / per_row as f32;
    let mut action = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = vec2(CARD_GAP, CARD_GAP);
        for chunk in cards.chunks(per_row) {
            ui.horizontal(|ui| {
                for card in chunk {
                    if card_ui(ui, card, card_width) {
                        action = Some(WelcomeAction::NewConnection(Some(card.id.clone())));
                    }
                }
            });
        }
    });
    action
}

/// One card: the database mark, the name and the detail. Its accessibility label is the name.
fn card_ui(ui: &mut egui::Ui, card: &DriverCard, width: f32) -> bool {
    let palette = theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(width, CARD_HEIGHT), Sense::click());
    let name = card.name.clone();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name.clone()));
    let highlighted = response.hovered() || response.has_focus();
    let fill = if highlighted {
        palette.bg_raised.lerp_to_gamma(palette.accent, 0.08)
    } else {
        palette.bg_raised
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(10),
        fill,
        Stroke::new(
            1.0,
            if highlighted {
                palette.accent
            } else {
                palette.border
            },
        ),
        StrokeKind::Inside,
    );
    let icon = Rect::from_min_size(rect.min + vec2(14.0, 12.0), vec2(26.0, 26.0));
    icons::paint_database_at(ui, icon, card.icon);
    let text_width = width - 28.0;
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
        FontId::proportional(10.5),
        palette.text_muted,
        text_width,
    );
    ui.painter()
        .galley(rect.min + vec2(14.0, 48.0), title, palette.text);
    ui.painter()
        .galley(rect.min + vec2(14.0, 70.0), detail, palette.text_muted);
    response
        .on_hover_text(format!("{} · {}", card.name, card.detail))
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
        width * 0.25,
    );
    let when_left = rect.right() - 14.0 - when.size().x;
    ui.painter().galley(
        pos2(when_left, y - when.size().y / 2.0),
        when,
        palette.text_muted,
    );
    let left = dot.x + Spacing::L;
    let room = (when_left - Spacing::L - left).max(1.0);
    let title = one_line(
        ui,
        &item.name,
        theme::semibold(ui.ctx(), 13.0),
        palette.text,
        room,
    );
    let detail = one_line(
        ui,
        &item.detail,
        FontId::proportional(11.5),
        palette.text_muted,
        room,
    );
    ui.painter()
        .galley(pos2(left, rect.top() + 9.0), title, palette.text);
    ui.painter()
        .galley(pos2(left, rect.top() + 30.0), detail, palette.text_muted);
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let response = if item.name.chars().count() > 40 {
        response.on_hover_text(&item.name)
    } else {
        response
    };
    response.clicked()
}
