//! The Triggers sub-tab and the form of a standalone trigger.

use egui::{ComboBox, TextEdit};
use sqlr_core::{SqlSyntax, TableRef, TriggerDef};

use super::indexes::table_picker;
use super::{ObjectEditorContext, caption, code_editor, expr_field, form_grid, master_detail};
use crate::theme::{Spacing, icons};

const TIMINGS: [&str; 3] = ["BEFORE", "AFTER", "INSTEAD OF"];
const EVENTS: [&str; 4] = ["INSERT", "UPDATE", "DELETE", "TRUNCATE"];

pub(super) fn new_trigger(table: &TableRef, name: String) -> TriggerDef {
    TriggerDef {
        name,
        table: table.clone(),
        timing: "BEFORE".into(),
        events: vec!["INSERT".into()],
        for_each_row: true,
        condition: None,
        body: String::new(),
    }
}

pub(super) fn triggers_tab(
    ui: &mut egui::Ui,
    triggers: &mut Vec<TriggerDef>,
    table: &TableRef,
    selected: &mut Option<usize>,
    syntax: &SqlSyntax,
) {
    ui.horizontal(|ui| {
        if icons::add_button(ui, "Add trigger").clicked() {
            let name = (1..)
                .map(|n| format!("trigger{n}"))
                .find(|n| !triggers.iter().any(|t| &t.name == n))
                .unwrap_or_default();
            triggers.push(new_trigger(table, name));
            *selected = Some(triggers.len() - 1);
        }
        if icons::delete_button(ui, selected.is_some(), "Delete trigger").clicked()
            && let Some(i) = selected.take()
        {
            triggers.remove(i);
        }
    });
    ui.add_space(Spacing::S);
    let names: Vec<String> = triggers
        .iter()
        .map(|t| format!("{} ({} {})", t.name, t.timing, t.events.join(" OR ")))
        .collect();
    master_detail(ui, "triggers", &names, selected, "No triggers", |ui, i| {
        trigger_form(ui, &mut triggers[i], syntax, "trigger");
    });
}

pub(super) fn trigger_form(ui: &mut egui::Ui, t: &mut TriggerDef, syntax: &SqlSyntax, id: &str) {
    form_grid(ui, id, |ui| {
        caption(ui, "Name");
        crate::text_menu::text_edit(ui, TextEdit::singleline(&mut t.name).desired_width(240.0));
        ui.end_row();
        caption(ui, "Timing");
        ComboBox::from_id_salt((id, "timing"))
            .selected_text(t.timing.clone())
            .width(140.0)
            .show_ui(ui, |ui| {
                for timing in TIMINGS {
                    if ui.selectable_label(t.timing == timing, timing).clicked() {
                        t.timing = timing.to_string();
                    }
                }
            });
        ui.end_row();
        caption(ui, "Events");
        ui.horizontal(|ui| {
            for event in EVENTS {
                let mut on = t.events.iter().any(|e| e.eq_ignore_ascii_case(event));
                if ui.checkbox(&mut on, event).changed() {
                    if on {
                        t.events.push(event.to_string());
                    } else {
                        t.events.retain(|e| !e.eq_ignore_ascii_case(event));
                    }
                }
            }
            // Events the checkboxes do not name (`UPDATE OF a`) stay as written.
            for other in t
                .events
                .iter()
                .filter(|e| !EVENTS.iter().any(|k| e.eq_ignore_ascii_case(k)))
            {
                caption(ui, other);
            }
        });
        ui.end_row();
        caption(ui, "For each row");
        ui.checkbox(&mut t.for_each_row, "");
        ui.end_row();
        caption(ui, "When");
        expr_field(ui, &mut t.condition, syntax, 360.0, "condition expression");
        ui.end_row();
    });
    ui.add_space(Spacing::S);
    caption(ui, "Action (a function call or statements, written as is)");
    code_editor(ui, &mut t.body, syntax, 8, "EXECUTE FUNCTION f()");
}

/// A standalone trigger: its table (chosen while it is new) and its fields.
pub(super) fn trigger_object_tab(ui: &mut egui::Ui, t: &mut TriggerDef, ctx: &ObjectEditorContext) {
    form_grid(ui, "trigger_table", |ui| {
        caption(ui, "Table");
        table_picker(ui, "trigger_table_pick", &mut t.table, ctx);
        ui.end_row();
    });
    trigger_form(ui, t, &ctx.syntax, "trigger_object");
}
