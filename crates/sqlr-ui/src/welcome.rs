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
    /// Its driver is not installed yet: choose a local driver file first.
    pub install: bool,
}

/// A recently used connection.
#[derive(Clone, Debug, PartialEq)]
pub struct RecentItem {
    pub id: String,
    pub name: String,
    /// "PostgreSQL · db.example", "SQLite · app.db".
    pub detail: String,
    /// The driver id, for its database mark.
    pub driver: String,
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
const COLUMN: f32 = 880.0;
const TILE_HEIGHT: f32 = 56.0;
const TILE_GAP: f32 = 10.0;
/// Tiles narrower than this wrap onto another row.
const TILE_MIN: f32 = 156.0;
const RECENT_ROW: f32 = 52.0;

/// Shows the welcome screen; returns what the user picked. A returning user (with recent
/// connections) sees them first; a new user starts with the database tiles.
pub fn welcome(
    ui: &mut egui::Ui,
    cards: &[DriverCard],
    recent: &[RecentItem],
) -> Option<WelcomeAction> {
    let mut action = None;
    egui::ScrollArea::vertical()
        .id_salt("welcome")
        .auto_shrink(false)
        .show(ui, |ui| {
            let available = ui.available_width();
            let narrow = available < 600.0;
            let margin = if narrow { 16.0 } else { 36.0 };
            let width = (available - 2.0 * margin).clamp(1.0, COLUMN);
            let side = ((available - width) / 2.0).max(0.0);
            ui.add_space(if narrow { 24.0 } else { 44.0 });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(side);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.spacing_mut().item_spacing.x = Spacing::M;
                    header(ui, width);
                    ui.add_space(28.0);
                    let mut pick = |a: Option<WelcomeAction>| {
                        if a.is_some() {
                            action = a;
                        }
                    };
                    if recent.is_empty() {
                        pick(section_with_button(
                            ui,
                            width,
                            "Create a connection",
                            "Choose a database to get started.",
                        ));
                        ui.add_space(14.0);
                        pick(tile_grid(ui, cards, width));
                        ui.add_space(24.0);
                        pick(quick_start(ui, width));
                    } else {
                        pick(section_with_button(
                            ui,
                            width,
                            "Recent connections",
                            "Pick up where you left off.",
                        ));
                        ui.add_space(12.0);
                        pick(recent_list(ui, recent, width));
                        ui.add_space(32.0);
                        section(ui, "Start something new", "Connect another database.");
                        ui.add_space(14.0);
                        pick(tile_grid(ui, cards, width));
                        ui.add_space(24.0);
                        pick(quick_start(ui, width));
                    }
                    ui.add_space(32.0);
                });
            });
        });
    action
}

/// A section title with the primary "New connection…" on its right (below it when narrow).
fn section_with_button(
    ui: &mut egui::Ui,
    width: f32,
    title: &str,
    subtitle: &str,
) -> Option<WelcomeAction> {
    let button = theme::primary_button(ui.ctx(), "New connection…");
    let clicked = if width < 480.0 {
        section(ui, title, subtitle);
        ui.add_space(8.0);
        ui.add_sized(vec2(136.0, 30.0), button).clicked()
    } else {
        ui.horizontal(|ui| {
            section(ui, title, subtitle);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_sized(vec2(136.0, 30.0), button).clicked()
            })
            .inner
        })
        .inner
    };
    clicked.then_some(WelcomeAction::NewConnection(None))
}

/// The palettes' shortcuts, from the keymap: everything else is a search away. Key caps sit
/// in one right-aligned column, their labels after them.
fn key_hints(ui: &mut egui::Ui) {
    let palette = theme::palette(ui.ctx());
    let hints: Vec<(String, &str)> = [
        (sqlr_core::ActionId::Commands, "Commands"),
        (sqlr_core::ActionId::OpenObject, "Go to a table"),
    ]
    .into_iter()
    .filter_map(|(action, label)| Some((crate::keymap::shortcut(ui.ctx(), action)?, label)))
    .collect();
    // Right-aligned rows, the key caps last: they line up on the column's right edge.
    ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
        ui.spacing_mut().item_spacing.y = Spacing::S;
        for (keys, label) in hints {
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 22.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    crate::keymap_editor::keycaps(ui, &[keys]);
                    ui.label(RichText::new(label).size(12.0).color(palette.text_muted));
                },
            );
        }
    });
}

/// A compact brand header that leaves the connection choices in focus.
fn header(ui: &mut egui::Ui, width: f32) {
    let palette = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        let (mark, _) = ui.allocate_exact_size(vec2(52.0, 52.0), Sense::hover());
        ui.painter().image(
            app_icon_texture(ui.ctx()),
            mark,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                RichText::new("sqlr")
                    .font(theme::semibold(ui.ctx(), 28.0))
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
                ui.allocate_ui_with_layout(
                    vec2(220.0, 52.0),
                    egui::Layout::top_down(egui::Align::Max),
                    key_hints,
                );
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

/// Two quiet entry points for exploring sqlr and bringing over an existing workspace. The demo
/// is the suggested one: an accent wash and an accent button.
fn quick_start(ui: &mut egui::Ui, width: f32) -> Option<WelcomeAction> {
    let palette = theme::palette(ui.ctx());
    let mut action = None;
    let columns = if width >= 600.0 { 2 } else { 1 };
    let card_width = (width - 12.0 * (columns - 1) as f32) / columns as f32;
    let mut tile = |ui: &mut egui::Ui, demo: bool| {
        let (fill, border) = if demo {
            (palette.accent_soft, palette.accent.gamma_multiply(0.55))
        } else {
            (palette.bg_raised, palette.border)
        };
        egui::Frame::new()
            .fill(fill)
            .stroke(Stroke::new(1.0, border))
            .corner_radius(10)
            .inner_margin(16)
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width((card_width - 34.0).max(1.0));
                    ui.horizontal(|ui| {
                        let icon = if demo { icons::RUN } else { icons::ARRIVE };
                        let color = if demo {
                            palette.accent
                        } else {
                            palette.text_muted
                        };
                        ui.label(icons::text(ui, icon, 16.0).color(color));
                        ui.label(
                            RichText::new(if demo {
                                "Just exploring?"
                            } else {
                                "Coming from another client?"
                            })
                            .font(theme::semibold(ui.ctx(), 14.0))
                            .color(palette.text),
                        );
                    });
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(if demo {
                            "Try sqlr on a ready-to-query sample database."
                        } else {
                            "Bring your saved connections from DBeaver, DataGrip and others."
                        })
                        .size(12.0)
                        .color(palette.text_muted),
                    );
                    ui.add_space(10.0);
                    if demo {
                        let button = egui::Button::new(
                            RichText::new("Open demo database").color(palette.accent_text),
                        )
                        .fill(palette.accent);
                        if ui.add(button).clicked() {
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

/// The recent connections in one framed list.
fn recent_list(ui: &mut egui::Ui, recent: &[RecentItem], width: f32) -> Option<WelcomeAction> {
    let palette = theme::palette(ui.ctx());
    let mut action = None;
    egui::Frame::new()
        .fill(palette.bg_raised)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(10)
        .inner_margin(6)
        .show(ui, |ui| {
            ui.set_width((width - 14.0).max(1.0));
            ui.spacing_mut().item_spacing.y = 2.0;
            for item in recent {
                if recent_row(ui, item, ui.available_width()) {
                    action = Some(WelcomeAction::OpenRecent(item.id.clone()));
                }
            }
        });
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
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(
            RichText::new(title)
                .font(theme::semibold(ui.ctx(), 17.0))
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

/// The database tiles, as many per row as fit, and "More databases…" as the last tile.
fn tile_grid(ui: &mut egui::Ui, cards: &[DriverCard], width: f32) -> Option<WelcomeAction> {
    let count = cards.len() + 1;
    let fit = (((width + TILE_GAP) / (TILE_MIN + TILE_GAP)).floor() as usize).max(1);
    // Balanced rows: as few rows as fit allows, then as even as possible.
    let rows = count.div_ceil(fit);
    let per_row = count.div_ceil(rows).clamp(1, fit);
    let tile_width = (width - TILE_GAP * (per_row as f32 - 1.0)) / per_row as f32;
    let mut action = None;
    let tiles: Vec<Option<&DriverCard>> = cards.iter().map(Some).chain([None]).collect();
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = vec2(TILE_GAP, TILE_GAP);
        for chunk in tiles.chunks(per_row) {
            ui.horizontal(|ui| {
                for tile in chunk {
                    match tile {
                        Some(card) => {
                            if card_ui(ui, card, tile_width) {
                                action = Some(WelcomeAction::NewConnection(Some(card.id.clone())));
                            }
                        }
                        None => {
                            if more_tile(ui, tile_width) {
                                action = Some(WelcomeAction::MoreDatabases);
                            }
                        }
                    }
                }
            });
        }
    });
    action
}

/// The frame of a tile: raised, bordered, the accent on hover or focus.
fn tile_frame(ui: &mut egui::Ui, width: f32, label: &str) -> (Rect, egui::Response, bool) {
    let palette = theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(width, TILE_HEIGHT), Sense::click());
    let label = label.to_string();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label.clone()));
    let highlighted = response.hovered() || response.has_focus();
    let fill = if highlighted {
        palette.bg_raised.lerp_to_gamma(palette.accent, 0.08)
    } else {
        palette.bg_raised
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
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
    (rect, response, highlighted)
}

/// The name and detail of a tile, right of its 28 px mark.
fn tile_text(ui: &egui::Ui, rect: Rect, name: &str, detail: &str, detail_color: Color32) {
    let palette = theme::palette(ui.ctx());
    let left = rect.left() + 12.0 + 28.0 + 10.0;
    let room = rect.right() - 10.0 - left;
    let title = one_line(
        ui,
        name,
        theme::semibold(ui.ctx(), 13.0),
        palette.text,
        room,
    );
    let detail = one_line(ui, detail, FontId::proportional(11.0), detail_color, room);
    ui.painter()
        .galley(pos2(left, rect.top() + 11.0), title, palette.text);
    ui.painter()
        .galley(pos2(left, rect.top() + 30.0), detail, detail_color);
}

/// One database tile: its mark, name and detail. An installable one says it downloads its
/// driver. Its accessibility label is the name.
fn card_ui(ui: &mut egui::Ui, card: &DriverCard, width: f32) -> bool {
    let palette = theme::palette(ui.ctx());
    let (rect, response, _) = tile_frame(ui, width, &card.name);
    let mark = Rect::from_center_size(pos2(rect.left() + 26.0, rect.center().y), vec2(26.0, 26.0));
    icons::paint_database_at(ui, mark, card.icon);
    if card.install {
        // The download glyph comes from the icon font: the detail is painted in two parts.
        tile_text(ui, rect, &card.name, "", palette.accent);
        let left = rect.left() + 50.0;
        let y = rect.top() + 37.0;
        ui.painter().text(
            pos2(left, y),
            egui::Align2::LEFT_CENTER,
            icons::DOWNLOAD,
            icons::font(ui, 12.0),
            palette.accent,
        );
        ui.painter().text(
            pos2(left + 16.0, y),
            egui::Align2::LEFT_CENTER,
            "Driver required",
            FontId::proportional(11.0),
            palette.accent,
        );
    } else {
        tile_text(ui, rect, &card.name, &card.detail, palette.text_muted);
    }
    let hover = if card.install {
        format!(
            "{}: choose a driver file, then set up a connection",
            card.name
        )
    } else {
        format!("{} · {}", card.name, card.detail)
    };
    response
        .on_hover_text(hover)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// The last tile: every other database sqlr knows.
fn more_tile(ui: &mut egui::Ui, width: f32) -> bool {
    let palette = theme::palette(ui.ctx());
    let (rect, response, highlighted) = tile_frame(ui, width, "More databases…");
    let mark = Rect::from_center_size(pos2(rect.left() + 26.0, rect.center().y), vec2(28.0, 28.0));
    ui.painter()
        .rect_filled(mark, CornerRadius::same(6), palette.bg_subtle);
    ui.painter().text(
        mark.center(),
        egui::Align2::CENTER_CENTER,
        icons::ADD,
        icons::font(ui, 15.0),
        if highlighted {
            palette.accent
        } else {
            palette.text_muted
        },
    );
    tile_text(
        ui,
        rect,
        "More databases…",
        "Browse every engine",
        palette.text_muted,
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// One recent connection: its database mark (with a color bar when it has one), name, detail,
/// and how long ago on the right. The whole row is one button labeled with the name.
fn recent_row(ui: &mut egui::Ui, item: &RecentItem, width: f32) -> bool {
    let palette = theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(width, RECENT_ROW), Sense::click());
    let name = item.name.clone();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name.clone()));
    if response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(Spacing::RADIUS_M), palette.hover);
    }
    let y = rect.center().y;
    if let Some(color) = item.color {
        let bar = Rect::from_center_size(pos2(rect.left() + 4.0, y), vec2(3.0, 24.0));
        ui.painter().rect_filled(bar, CornerRadius::same(2), color);
    }
    let mark = Rect::from_center_size(pos2(rect.left() + 24.0, y), vec2(22.0, 22.0));
    icons::paint_database_at(ui, mark, &item.driver);
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
    let left = mark.right() + Spacing::L;
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
        .galley(pos2(left, rect.top() + 8.0), title, palette.text);
    ui.painter()
        .galley(pos2(left, rect.top() + 28.0), detail, palette.text_muted);
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let response = if item.name.chars().count() > 40 {
        response.on_hover_text(&item.name)
    } else {
        response
    };
    response.clicked()
}
