//! The Definition sub-tab of a view or materialized view: its query and column names.

use egui::TextEdit;
use sqlr_core::{ObjectEditing, ViewDef};

use super::{ObjectEditorContext, caption, code_editor, form_grid};
use crate::theme::Spacing;

pub(super) fn view_tab(
    ui: &mut egui::Ui,
    v: &mut ViewDef,
    _editing: &ObjectEditing,
    ctx: &ObjectEditorContext,
) {
    form_grid(ui, "view", |ui| {
        caption(ui, "Column names");
        let mut columns = v.columns.join(", ");
        let response = crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut columns)
                .desired_width(420.0)
                .hint_text("as the query names them"),
        );
        if response.changed() {
            v.columns = columns
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        ui.end_row();
    });
    ui.add_space(Spacing::S);
    caption(ui, "Query");
    code_editor(ui, &mut v.query, &ctx.syntax, 14, "SELECT …");
    if v.materialized && !v.indexes.is_empty() {
        let names: Vec<&str> = v.indexes.iter().map(|i| i.name.as_str()).collect();
        caption(
            ui,
            &format!("Indexes (recreated with the view): {}", names.join(", ")),
        );
    }
}
