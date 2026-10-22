//! The Foreign keys sub-tab: the target table is picked from the catalog, then column pairs
//! and actions.

use egui::ComboBox;
use sqlr_core::{ConstraintDef, FkAction, TableDef, TableRef};

use super::indexes::table_picker;
use super::{
    ObjectEditorAction, ObjectEditorContext, caption, form_grid, master_detail, optional_text,
    sql_view,
};
use crate::theme::{self, Spacing, icons};

const ACTIONS: [FkAction; 5] = [
    FkAction::NoAction,
    FkAction::Restrict,
    FkAction::Cascade,
    FkAction::SetNull,
    FkAction::SetDefault,
];

fn label(c: &ConstraintDef) -> String {
    match c {
        ConstraintDef::ForeignKey {
            name,
            columns,
            target,
            ..
        } => format!(
            "{} ({}) → {}",
            name.as_deref().unwrap_or("unnamed"),
            columns.join(", "),
            if target.name.is_empty() {
                "?"
            } else {
                &target.name
            }
        ),
        other => other.name().unwrap_or("unnamed").to_string(),
    }
}

/// `selected` indexes `table.constraints`. Asks for the catalog's tables while it has none.
pub(super) fn foreign_keys_tab(
    ui: &mut egui::Ui,
    table: &mut TableDef,
    selected: &mut Option<usize>,
    ctx: &ObjectEditorContext,
) -> Option<ObjectEditorAction> {
    let p = theme::palette(ui.ctx());
    let shown: Vec<usize> = (0..table.constraints.len())
        .filter(|&i| table.constraints[i].is_foreign_key())
        .collect();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        if icons::button(ui, true, icons::ADD, Some(p.success), "Add foreign key").clicked() {
            // The first column outside the primary key is the likeliest reference.
            let pk = table.primary_key().unwrap_or(&[]).to_vec();
            let column = table
                .columns
                .iter()
                .find(|c| !pk.contains(&c.name))
                .or(table.columns.first())
                .map(|c| c.name.clone())
                .unwrap_or_default();
            table.constraints.push(ConstraintDef::ForeignKey {
                name: None,
                columns: vec![column],
                target: TableRef {
                    database: table.table.database.clone(),
                    schema: table.table.schema.clone(),
                    name: String::new(),
                },
                target_columns: vec![String::new()],
                on_update: FkAction::NoAction,
                on_delete: FkAction::NoAction,
                deferrable: false,
                initially_deferred: false,
            });
            *selected = Some(table.constraints.len() - 1);
        }
        let current = selected.filter(|i| shown.contains(i));
        if ui
            .add_enabled(current.is_some(), egui::Button::new("Delete foreign key"))
            .clicked()
            && let Some(i) = current
        {
            table.constraints.remove(i);
            *selected = None;
        }
    });
    ui.add_space(Spacing::S);
    let shown: Vec<usize> = (0..table.constraints.len())
        .filter(|&i| table.constraints[i].is_foreign_key())
        .collect();
    let names: Vec<String> = shown
        .iter()
        .map(|&i| label(&table.constraints[i]))
        .collect();
    let columns: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
    let mut position = selected.and_then(|i| shown.iter().position(|&s| s == i));
    master_detail(
        ui,
        "foreign_keys",
        &names,
        &mut position,
        "No foreign keys",
        |ui, k| fk_form(ui, &mut table.constraints[shown[k]], &columns, ctx),
    );
    *selected = position.map(|k| shown[k]);
    ctx.tables
        .is_empty()
        .then_some(ObjectEditorAction::PickTable)
}

fn action_combo(ui: &mut egui::Ui, id: &str, action: &mut FkAction) {
    ComboBox::from_id_salt(id)
        .selected_text(action.sql())
        .width(130.0)
        .show_ui(ui, |ui| {
            for a in ACTIONS {
                ui.selectable_value(action, a, a.sql());
            }
        });
}

fn column_combo(ui: &mut egui::Ui, id: (&str, usize), value: &mut String, options: &[String]) {
    let text = if value.is_empty() {
        "Choose…".to_string()
    } else {
        value.clone()
    };
    ComboBox::from_id_salt(id)
        .selected_text(text)
        .width(160.0)
        .show_ui(ui, |ui| {
            for o in options {
                if ui.selectable_label(value == o, o).clicked() {
                    *value = o.clone();
                }
            }
        });
}

fn fk_form(
    ui: &mut egui::Ui,
    c: &mut ConstraintDef,
    columns: &[String],
    ctx: &ObjectEditorContext,
) {
    let ConstraintDef::ForeignKey {
        name,
        columns: from,
        target,
        target_columns,
        on_update,
        on_delete,
        deferrable,
        initially_deferred,
    } = c
    else {
        if let ConstraintDef::Raw { definition, .. } = c {
            caption(ui, "Written as the server prints it:");
            sql_view(ui, "raw_fk", definition, &ctx.syntax, 120.0);
        }
        return;
    };
    let mut retargeted = false;
    form_grid(ui, "foreign_key", |ui| {
        caption(ui, "Name");
        optional_text(ui, name, 240.0, "generated by the server");
        ui.end_row();
        caption(ui, "Target table");
        retargeted = table_picker(ui, "fk_target", target, ctx);
        ui.end_row();
        caption(ui, "On update");
        action_combo(ui, "fk_on_update", on_update);
        ui.end_row();
        caption(ui, "On delete");
        action_combo(ui, "fk_on_delete", on_delete);
        ui.end_row();
        caption(ui, "Deferrable");
        ui.horizontal(|ui| {
            ui.checkbox(deferrable, "");
            ui.add_enabled(
                *deferrable,
                egui::Checkbox::new(initially_deferred, "initially deferred"),
            );
        });
        ui.end_row();
    });
    if !*deferrable {
        *initially_deferred = false;
    }
    let target_options = ctx
        .columns_of(target)
        .map(<[String]>::to_vec)
        .unwrap_or_default();
    if retargeted {
        // Each pair points at the target column of the same name, else the first one.
        target_columns.resize(from.len(), String::new());
        for (local, remote) in from.iter().zip(target_columns.iter_mut()) {
            *remote = target_options
                .iter()
                .find(|t| *t == local)
                .or(target_options.first())
                .cloned()
                .unwrap_or_default();
        }
    }
    ui.add_space(Spacing::S);
    caption(ui, "Columns → target columns");
    let mut remove = None;
    for k in 0..from.len() {
        ui.horizontal(|ui| {
            column_combo(ui, ("fk_local", k), &mut from[k], columns);
            ui.label("→");
            // No target columns means the target's primary key (SQLite); only a pick
            // writes them.
            let mut remote = target_columns.get(k).cloned().unwrap_or_default();
            if target_columns.is_empty() {
                caption(ui, "primary key");
            }
            column_combo(ui, ("fk_remote", k), &mut remote, &target_options);
            if target_columns.get(k) != Some(&remote) && !remote.is_empty() {
                target_columns.resize(from.len(), String::new());
                target_columns[k] = remote;
            }
            if from.len() > 1 && ui.small_button("Remove").clicked() {
                remove = Some(k);
            }
        });
    }
    if let Some(k) = remove {
        from.remove(k);
        if k < target_columns.len() {
            target_columns.remove(k);
        }
    }
    if ui.button("Add column pair").clicked() {
        from.push(columns.first().cloned().unwrap_or_default());
        target_columns.push(target_options.first().cloned().unwrap_or_default());
    }
}
