//! The Indexes sub-tab and the form of a standalone index.

use egui::{ComboBox, TextEdit};
use sqlr_core::{IndexColumn, IndexDef, SqlSyntax, TableRef};

use super::{
    ObjectEditorContext, caption, expr_field, form_grid, master_detail, optional_text, sql_view,
};
use crate::theme::{self, Spacing, icons};

fn index_label(i: &IndexDef) -> String {
    let unique = if i.unique { "unique " } else { "" };
    let columns: Vec<&str> = i.columns.iter().map(|c| c.expr.as_str()).collect();
    format!("{} ({unique}{})", i.name, columns.join(", "))
}

pub(super) fn indexes_tab(
    ui: &mut egui::Ui,
    indexes: &mut Vec<IndexDef>,
    selected: &mut Option<usize>,
    columns: &[String],
    syntax: &SqlSyntax,
) {
    ui.horizontal(|ui| {
        if icons::add_button(ui, "Add index").clicked() {
            let name = (1..)
                .map(|n| format!("index{n}"))
                .find(|n| !indexes.iter().any(|i| &i.name == n))
                .unwrap_or_default();
            indexes.push(IndexDef {
                name,
                columns: columns
                    .first()
                    .map(|c| vec![IndexColumn::column(c)])
                    .unwrap_or_default(),
                ..Default::default()
            });
            *selected = Some(indexes.len() - 1);
        }
        if icons::delete_button(ui, selected.is_some(), "Delete index").clicked()
            && let Some(i) = selected.take()
        {
            indexes.remove(i);
        }
    });
    ui.add_space(Spacing::S);
    let names: Vec<String> = indexes.iter().map(index_label).collect();
    master_detail(ui, "indexes", &names, selected, "No indexes", |ui, i| {
        index_form(ui, &mut indexes[i], columns, syntax, "index");
    });
}

/// The fields of one index; `columns` are the table's columns.
pub(super) fn index_form(
    ui: &mut egui::Ui,
    index: &mut IndexDef,
    columns: &[String],
    syntax: &SqlSyntax,
    id: &str,
) {
    if let Some(definition) = index.definition.clone() {
        form_grid(ui, id, |ui| {
            caption(ui, "Name");
            crate::text_menu::text_edit(
                ui,
                TextEdit::singleline(&mut index.name).desired_width(240.0),
            );
            ui.end_row();
        });
        caption(
            ui,
            "This index uses options the form cannot show; it is written as the server prints it:",
        );
        sql_view(ui, id, &definition, syntax, 120.0);
        return;
    }
    form_grid(ui, id, |ui| {
        caption(ui, "Name");
        crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut index.name).desired_width(240.0),
        );
        ui.end_row();
        caption(ui, "Unique");
        ui.checkbox(&mut index.unique, "");
        ui.end_row();
        caption(ui, "Method");
        optional_text(ui, &mut index.method, 160.0, "default");
        ui.end_row();
        caption(ui, "Where");
        expr_field(
            ui,
            &mut index.predicate,
            syntax,
            320.0,
            "predicate expression",
        );
        ui.end_row();
        caption(ui, "Include");
        let mut include = index.include.join(", ");
        let response = crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut include)
                .desired_width(320.0)
                .hint_text("columns, comma separated"),
        );
        if response.changed() {
            index.include = include
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        ui.end_row();
    });
    ui.add_space(Spacing::S);
    caption(
        ui,
        "Key columns (a column name, or an expression in parentheses)",
    );
    let mut remove = None;
    for (k, part) in index.columns.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let response = crate::text_menu::text_edit(
                ui,
                TextEdit::singleline(&mut part.expr)
                    .desired_width(200.0)
                    .font(theme::monospace(ui.ctx())),
            );
            if response.changed() {
                part.expression = !columns.contains(&part.expr)
                    && part.expr.starts_with('(')
                    && part.expr.ends_with(')');
            }
            ComboBox::from_id_salt((id, "key", k))
                .selected_text("column")
                .width(90.0)
                .show_ui(ui, |ui| {
                    for c in columns {
                        if ui.selectable_label(part.expr == *c, c).clicked() {
                            part.expr = c.clone();
                            part.expression = false;
                        }
                    }
                });
            ui.checkbox(&mut part.desc, "descending");
            if icons::row_button(ui, icons::CLOSE, "Remove").clicked() {
                remove = Some(k);
            }
        });
    }
    if let Some(k) = remove {
        index.columns.remove(k);
    }
    if ui.button("Add key column").clicked() {
        let next = columns
            .iter()
            .find(|c| !index.columns.iter().any(|p| &p.expr == *c))
            .or(columns.first())
            .cloned()
            .unwrap_or_default();
        index.columns.push(IndexColumn::column(&next));
    }
}

/// A standalone index: its table (chosen while it is new) and its fields.
pub(super) fn index_object_tab(
    ui: &mut egui::Ui,
    table: &mut TableRef,
    index: &mut IndexDef,
    ctx: &ObjectEditorContext,
) {
    form_grid(ui, "index_table", |ui| {
        caption(ui, "Table");
        table_picker(ui, "index_table_pick", table, ctx);
        ui.end_row();
    });
    let columns = ctx
        .columns_of(table)
        .map(<[String]>::to_vec)
        .unwrap_or_default();
    index_form(ui, index, &columns, &ctx.syntax, "index_object");
}

/// A ComboBox over the catalog's tables; keeps the schema of `table` when it has one.
pub(super) fn table_picker(
    ui: &mut egui::Ui,
    id: &str,
    table: &mut TableRef,
    ctx: &ObjectEditorContext,
) -> bool {
    let mut changed = false;
    let text = if table.name.is_empty() {
        "Choose a table…".to_string()
    } else {
        table.name.clone()
    };
    ComboBox::from_id_salt(id)
        .selected_text(text)
        .width(240.0)
        .show_ui(ui, |ui| {
            if ctx.tables.is_empty() {
                caption(ui, "Loading tables…");
            }
            for (t, _) in ctx.tables {
                let label = match &t.schema {
                    Some(s) if table.schema.as_ref() != Some(s) => format!("{s}.{}", t.name),
                    _ => t.name.clone(),
                };
                if ui.selectable_label(*t == *table, label).clicked() {
                    *table = t.clone();
                    changed = true;
                }
            }
        });
    changed
}
