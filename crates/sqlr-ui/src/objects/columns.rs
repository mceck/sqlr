//! The Columns sub-tab: a virtualized table of a table's columns (only visible rows are
//! built, so a thousand columns stay cheap), with add, duplicate, delete and move.

use egui::{ComboBox, RichText, TextEdit};
use sqlr_core::{ColumnDef, Identity, ObjectEditing, SqlSyntax, TableDef};

use super::{caption, expr_field, known_type, optional_text};
use crate::theme::{self, Spacing, icons};

/// Row height of the column table.
pub(crate) const ROW: f32 = 26.0;

const W_NUM: f32 = 28.0;
const W_NAME: f32 = 150.0;
const W_TYPE: f32 = 130.0;
const W_NULL: f32 = 44.0;
/// The type field and its picker button.
const W_TYPE_CELL: f32 = W_TYPE + 20.0 + Spacing::S;
const W_EXPR: f32 = 140.0;
const W_IDENTITY: f32 = 110.0;
const W_COMMENT: f32 = 180.0;

fn identity_label(identity: Option<Identity>) -> &'static str {
    match identity {
        None => "—",
        Some(Identity::Always) => "Always",
        Some(Identity::ByDefault) => "By default",
        Some(Identity::AutoIncrement) => "Auto increment",
    }
}

/// A column name not used by `table` yet.
fn fresh_name(table: &TableDef, base: &str) -> String {
    (1..)
        .map(|n| format!("{base}{n}"))
        .find(|name| table.column(name).is_none())
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn columns_tab(
    ui: &mut egui::Ui,
    table: &mut TableDef,
    selected: &mut Option<usize>,
    scroll_to: &mut Option<usize>,
    editing: &ObjectEditing,
    column_order: bool,
    syntax: &SqlSyntax,
) {
    if selected.is_some_and(|i| i >= table.columns.len()) {
        *selected = None;
    }
    let p = theme::palette(ui.ctx());
    // Row actions.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        if icons::add_button(ui, "Add column").clicked() {
            let column = ColumnDef {
                name: fresh_name(table, "column"),
                type_name: editing
                    .column_types
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "text".into()),
                nullable: true,
                ..Default::default()
            };
            let at = selected.map_or(table.columns.len(), |i| i + 1);
            table.columns.insert(at, column);
            *selected = Some(at);
            *scroll_to = Some(at);
        }
        let has = selected.is_some();
        if icons::short_button(ui, has, icons::DUPLICATE, None, "Duplicate", "Duplicate").clicked()
            && let Some(i) = *selected
        {
            let mut copy = table.columns[i].clone();
            copy.name = fresh_name(table, &format!("{}_", copy.name));
            copy.identity = None;
            table.columns.insert(i + 1, copy);
            *selected = Some(i + 1);
        }
        if icons::delete_button(ui, has, "Delete").clicked()
            && let Some(i) = *selected
        {
            table.columns.remove(i);
            *selected = (!table.columns.is_empty()).then(|| i.min(table.columns.len() - 1));
        }
        ui.separator();
        let why = "This database keeps columns in the order they were added";
        let up = selected.is_some_and(|i| i > 0);
        let up = icons::short_button(
            ui,
            column_order && up,
            icons::MOVE_UP,
            None,
            "Up",
            "Move up",
        )
        .on_disabled_hover_text(if column_order { "" } else { why });
        if up.clicked()
            && let Some(i) = *selected
        {
            table.columns.swap(i, i - 1);
            *selected = Some(i - 1);
        }
        let down = selected.is_some_and(|i| i + 1 < table.columns.len());
        let down = icons::short_button(
            ui,
            column_order && down,
            icons::MOVE_DOWN,
            None,
            "Down",
            "Move down",
        )
        .on_disabled_hover_text(if column_order { "" } else { why });
        if down.clicked()
            && let Some(i) = *selected
        {
            table.columns.swap(i, i + 1);
            *selected = Some(i + 1);
        }
        caption(ui, &format!("{} columns", table.columns.len()));
    });
    ui.add_space(Spacing::S);

    // Header band.
    let header = ui.available_width();
    egui::Frame::new()
        .fill(p.bg_subtle)
        .corner_radius(Spacing::RADIUS_S)
        .show(ui, |ui| {
            ui.set_min_width(header - 2.0 * ui.spacing().item_spacing.x);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = Spacing::S;
                let head = |ui: &mut egui::Ui, text: &str, width: f32| {
                    cell(ui, width, 22.0, |ui| {
                        ui.label(
                            RichText::new(text.to_uppercase())
                                .small()
                                .color(p.text_muted),
                        );
                    });
                };
                head(ui, "#", W_NUM);
                head(ui, "Name", W_NAME);
                head(ui, "Type", W_TYPE_CELL);
                head(ui, "Null", W_NULL);
                head(ui, "Default", W_EXPR);
                head(ui, "Generated", W_EXPR);
                head(ui, "Identity", W_IDENTITY);
                if editing.comments {
                    head(ui, "Comment", W_COMMENT);
                }
            });
        });

    let mut area = egui::ScrollArea::both()
        .id_salt("object_columns")
        .auto_shrink([false, false]);
    if let Some(row) = scroll_to.take() {
        area = area.vertical_scroll_offset(row as f32 * ROW);
    }
    let names: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
    // Rows touch, so row `i` starts at `i * ROW` (what `scroll_to` relies on).
    ui.spacing_mut().item_spacing.y = 0.0;
    area.show_rows(ui, ROW, table.columns.len(), |ui, range| {
        for i in range {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), ROW),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_height(ROW);
                    ui.spacing_mut().item_spacing.x = Spacing::S;
                    let duplicate = names[..i].contains(&names[i]);
                    column_row(
                        ui,
                        i,
                        &mut table.columns[i],
                        selected,
                        duplicate,
                        editing,
                        syntax,
                    );
                },
            );
        }
    });
}

/// A fixed-width, vertically centered cell, so header and rows line up.
fn cell(ui: &mut egui::Ui, width: f32, height: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.x = Spacing::S;
            add(ui);
        },
    );
}

fn column_row(
    ui: &mut egui::Ui,
    i: usize,
    c: &mut ColumnDef,
    selected: &mut Option<usize>,
    duplicate: bool,
    editing: &ObjectEditing,
    syntax: &SqlSyntax,
) {
    let p = theme::palette(ui.ctx());
    // The row's background sits under its fields: selection, then hover, then a hairline.
    let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), ROW));
    let is_selected = *selected == Some(i);
    let fill = if is_selected {
        p.accent_soft
    } else if ui.rect_contains_pointer(rect) {
        p.bg_subtle
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Spacing::RADIUS_S, fill);
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        egui::Stroke::new(1.0, p.border),
    );
    // Fields are flat until hovered or focused.
    let visuals = ui.visuals_mut();
    visuals.extreme_bg_color = egui::Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
    visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;

    cell(ui, W_NUM, ROW, |ui| {
        let number = ui.add(
            egui::Button::new(RichText::new((i + 1).to_string()).color(p.text_muted))
                .frame(false)
                .min_size(egui::vec2(W_NUM, ROW - 4.0)),
        );
        if number.clicked() {
            *selected = Some(i);
        }
    });
    cell(ui, W_NAME, ROW, |ui| {
        let bad_name = c.name.trim().is_empty() || duplicate;
        let mut name = TextEdit::singleline(&mut c.name)
            .desired_width(W_NAME)
            .hint_text("name");
        if bad_name {
            name = name.text_color(p.error);
        }
        let response = crate::text_menu::text_edit(ui, name);
        if response.gained_focus() {
            *selected = Some(i);
        }
        if bad_name {
            response.on_hover_text(if duplicate {
                "Another column has this name"
            } else {
                "The name is empty"
            });
        }
    });
    cell(ui, W_TYPE_CELL, ROW, |ui| {
        let known = known_type(&c.type_name, &editing.column_types);
        let mut type_edit = TextEdit::singleline(&mut c.type_name)
            .desired_width(W_TYPE)
            .font(theme::monospace(ui.ctx()));
        if !known {
            type_edit = type_edit.text_color(p.warning);
        }
        let response = crate::text_menu::text_edit(ui, type_edit);
        if response.gained_focus() {
            *selected = Some(i);
        }
        if !known {
            response.on_hover_text("Not a type this database suggests");
        }
        let caret = icons::text(ui, icons::CARET_DOWN, 12.0).color(p.text_muted);
        ui.menu_button(caret, |ui| {
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    for t in &editing.column_types {
                        if ui.button(t).clicked() {
                            c.type_name = t.clone();
                            ui.close();
                        }
                    }
                });
        });
    });
    cell(ui, W_NULL, ROW, |ui| {
        ui.checkbox(&mut c.nullable, "").on_hover_text("Nullable");
    });
    cell(ui, W_EXPR, ROW, |ui| {
        expr_field(ui, &mut c.default, syntax, W_EXPR, "default expression");
    });
    cell(ui, W_EXPR, ROW, |ui| {
        expr_field(ui, &mut c.generated, syntax, W_EXPR, "generated expression");
    });
    cell(ui, W_IDENTITY, ROW, |ui| {
        ComboBox::from_id_salt(("column_identity", i))
            .width(W_IDENTITY - 22.0)
            .selected_text(identity_label(c.identity))
            .show_ui(ui, |ui| {
                for identity in [
                    None,
                    Some(Identity::Always),
                    Some(Identity::ByDefault),
                    Some(Identity::AutoIncrement),
                ] {
                    ui.selectable_value(&mut c.identity, identity, identity_label(identity));
                }
            });
    });
    if editing.comments {
        cell(ui, W_COMMENT, ROW, |ui| {
            optional_text(ui, &mut c.comment, W_COMMENT, "comment");
        });
    }
}
