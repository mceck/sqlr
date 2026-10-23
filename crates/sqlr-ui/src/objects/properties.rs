//! The Properties sub-tab (owner, comment, driver options), the Partitions sub-tab
//! (PostgreSQL: attach and detach existing tables) and the sequence form.

use egui::TextEdit;
use sqlr_core::{ObjectDef, ObjectEditing, PartitionDef, SequenceDef, TableDef, TableRef};

use super::{ObjectEditorContext, caption, form_grid, optional_text};
use crate::theme::{self, Spacing, icons};

pub(super) fn properties_tab(ui: &mut egui::Ui, def: &mut ObjectDef, editing: &ObjectEditing) {
    match def {
        ObjectDef::Table(t) => {
            comment_row(ui, &mut t.comment, editing);
            options(ui, &mut t.options);
        }
        ObjectDef::View(v) => {
            comment_row(ui, &mut v.comment, editing);
            options(ui, &mut v.options);
        }
        ObjectDef::Routine(r) => options(ui, &mut r.options),
        ObjectDef::Schema(s) => {
            form_grid(ui, "schema", |ui| {
                caption(ui, "Owner");
                optional_text(ui, &mut s.owner, 240.0, "the current role");
                ui.end_row();
            });
            comment_row(ui, &mut s.comment, editing);
        }
        ObjectDef::Database(d) => options(ui, &mut d.options),
        ObjectDef::Sequence(s) => comment_row(ui, &mut s.comment, editing),
        _ => {
            caption(ui, "No properties");
        }
    }
}

fn comment_row(ui: &mut egui::Ui, comment: &mut Option<String>, editing: &ObjectEditing) {
    if !editing.comments {
        return;
    }
    form_grid(ui, "comment", |ui| {
        caption(ui, "Comment");
        optional_text(ui, comment, 420.0, "no comment");
        ui.end_row();
    });
}

/// Driver options as key/value rows (engine, charset, tablespace, `STRICT`…).
fn options(ui: &mut egui::Ui, options: &mut Vec<(String, String)>) {
    ui.add_space(Spacing::S);
    caption(ui, "Options");
    let mut remove = None;
    egui::Grid::new("object_options")
        .num_columns(3)
        .spacing([Spacing::S, Spacing::S])
        .show(ui, |ui| {
            for (i, (key, value)) in options.iter_mut().enumerate() {
                crate::text_menu::text_edit(
                    ui,
                    TextEdit::singleline(key)
                        .desired_width(160.0)
                        .hint_text("option"),
                );
                crate::text_menu::text_edit(
                    ui,
                    TextEdit::singleline(value)
                        .desired_width(260.0)
                        .hint_text("value"),
                );
                if icons::row_button(ui, icons::CLOSE, "Remove").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
    if let Some(i) = remove {
        options.remove(i);
    }
    if ui.button("Add option").clicked() {
        options.push((String::new(), String::new()));
    }
}

/// Partitions of a partitioned table: detach one, or attach an existing table.
pub(super) fn partitions_tab(
    ui: &mut egui::Ui,
    t: &mut TableDef,
    attach: &mut (String, String),
    ctx: &ObjectEditorContext,
) {
    if let Some(key) = t.option("partition_by") {
        caption(ui, &format!("Partitioned by {key}"));
    }
    ui.add_space(Spacing::S);
    let mut detach = None;
    egui::Grid::new("object_partitions")
        .num_columns(3)
        .striped(true)
        .spacing([Spacing::L, Spacing::S])
        .show(ui, |ui| {
            for (i, p) in t.partitions.iter().enumerate() {
                ui.label(&p.table.name);
                ui.label(egui::RichText::new(&p.bound).font(theme::monospace(ui.ctx())));
                if ui.small_button("Detach").clicked() {
                    detach = Some(i);
                }
                ui.end_row();
            }
        });
    if let Some(i) = detach {
        t.partitions.remove(i);
    }
    ui.add_space(Spacing::M);
    caption(ui, "Attach an existing table");
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("attach_table")
            .selected_text(if attach.0.is_empty() {
                "Choose a table…".to_string()
            } else {
                attach.0.clone()
            })
            .width(200.0)
            .show_ui(ui, |ui| {
                for (table, _) in ctx.tables {
                    if table.name != t.table.name
                        && ui
                            .selectable_label(attach.0 == table.name, &table.name)
                            .clicked()
                    {
                        attach.0 = table.name.clone();
                    }
                }
            });
        crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut attach.1)
                .desired_width(280.0)
                .font(theme::monospace(ui.ctx()))
                .hint_text("FOR VALUES FROM (1) TO (10)"),
        );
        let ready = !attach.0.is_empty() && !attach.1.trim().is_empty();
        if ui.add_enabled(ready, egui::Button::new("Attach")).clicked() {
            t.partitions.push(PartitionDef {
                table: TableRef {
                    name: std::mem::take(&mut attach.0),
                    ..t.table.clone()
                },
                bound: std::mem::take(&mut attach.1).trim().to_string(),
            });
        }
    });
}

fn number(ui: &mut egui::Ui, value: &mut Option<i64>, hint: &str) {
    let mut text = value.map(|v| v.to_string()).unwrap_or_default();
    let ok = text.is_empty() || text.parse::<i64>().is_ok();
    let mut edit = TextEdit::singleline(&mut text)
        .desired_width(160.0)
        .hint_text(hint);
    if !ok {
        edit = edit.text_color(theme::palette(ui.ctx()).error);
    }
    if crate::text_menu::text_edit(ui, edit).changed() {
        if text.trim().is_empty() {
            *value = None;
        } else if let Ok(v) = text.trim().parse() {
            *value = Some(v);
        }
    }
}

/// A sequence's fields; only a new sequence changes them (existing ones are renamed and
/// commented only).
pub(super) fn sequence_tab(ui: &mut egui::Ui, s: &mut SequenceDef, editing: &ObjectEditing) {
    form_grid(ui, "sequence", |ui| {
        caption(ui, "Data type");
        optional_text(ui, &mut s.data_type, 160.0, "bigint");
        ui.end_row();
        caption(ui, "Start");
        number(ui, &mut s.start, "1");
        ui.end_row();
        caption(ui, "Increment");
        number(ui, &mut s.increment, "1");
        ui.end_row();
        caption(ui, "Minimum");
        number(ui, &mut s.min_value, "default");
        ui.end_row();
        caption(ui, "Maximum");
        number(ui, &mut s.max_value, "default");
        ui.end_row();
        caption(ui, "Cache");
        number(ui, &mut s.cache, "1");
        ui.end_row();
        caption(ui, "Cycle");
        ui.checkbox(&mut s.cycle, "");
        ui.end_row();
    });
    comment_row(ui, &mut s.comment, editing);
}
