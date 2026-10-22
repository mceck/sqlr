//! The Definition sub-tab of a function or procedure.

use egui::TextEdit;
use sqlr_core::{ObjectKind, RoutineDef};

use super::{ObjectEditorContext, caption, code_editor, form_grid, optional_text};
use crate::theme::{self, Spacing};

pub(super) fn routine_tab(ui: &mut egui::Ui, r: &mut RoutineDef, ctx: &ObjectEditorContext) {
    let mono = theme::monospace(ui.ctx());
    form_grid(ui, "routine", |ui| {
        caption(ui, "Arguments");
        crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut r.arguments)
                .desired_width(420.0)
                .font(mono.clone())
                .hint_text("a integer, b text"),
        );
        ui.end_row();
        if r.kind == ObjectKind::Function {
            caption(ui, "Returns");
            optional_text(ui, &mut r.returns, 240.0, "return type");
            ui.end_row();
        }
        caption(ui, "Language");
        optional_text(ui, &mut r.language, 160.0, "default");
        ui.end_row();
    });
    ui.add_space(Spacing::S);
    caption(ui, "Body (written as is)");
    code_editor(ui, &mut r.body, &ctx.syntax, 16, "");
}
