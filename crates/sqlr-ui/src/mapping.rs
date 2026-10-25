//! The column-mapping table of a data transfer: one row per source column with the target
//! column it goes to, an optional type to convert to and an optional transform.

use egui::{ComboBox, RichText, TextEdit};
use sqlr_core::{ColumnMapping, ColumnMeta, TableColumn, Transform, auto_mapping};

use crate::theme::{self, Spacing};

/// Builds the transform a menu entry stands for.
type MakeTransform = fn() -> Option<Transform>;

const TRANSFORMS: [(&str, MakeTransform); 6] = [
    ("None", || None),
    ("Trim", || Some(Transform::Trim)),
    ("Upper case", || Some(Transform::Upper)),
    ("Lower case", || Some(Transform::Lower)),
    ("Empty → NULL", || Some(Transform::NullIfEmpty)),
    ("Constant", || Some(Transform::Constant(String::new()))),
];

fn transform_label(transform: &Option<Transform>) -> &'static str {
    match transform {
        None => "None",
        Some(Transform::Trim) => "Trim",
        Some(Transform::Upper) => "Upper case",
        Some(Transform::Lower) => "Lower case",
        Some(Transform::NullIfEmpty) => "Empty → NULL",
        Some(Transform::Constant(_)) => "Constant",
    }
}

/// Draws the mapping of `source` columns onto `targets` (the columns of an existing table;
/// empty for a new table, where each column keeps an editable name of its own). A mapping
/// that does not cover the source is replaced by one by name. Returns `true` when the user
/// changed it.
pub fn mapping_table(
    ui: &mut egui::Ui,
    source: &[ColumnMeta],
    targets: &[TableColumn],
    mapping: &mut Vec<ColumnMapping>,
) -> bool {
    let mut changed = false;
    let remap = ui.button("Auto-map by name").clicked();
    if mapping.len() != source.len() || remap {
        let names: Vec<String> = source.iter().map(|c| c.name.clone()).collect();
        *mapping = if targets.is_empty() {
            names
                .iter()
                .enumerate()
                .map(|(i, n)| ColumnMapping::new(i, Some(n.clone())))
                .collect()
        } else {
            let target_names: Vec<String> = targets.iter().map(|t| t.name.clone()).collect();
            auto_mapping(&names, &target_names)
        };
        changed = true;
    }
    let mut all = mapping.iter().all(|m| m.target.is_some());
    if ui.checkbox(&mut all, "Include all columns").changed() {
        for (index, m) in mapping.iter_mut().enumerate() {
            m.target = if all {
                targets
                    .iter()
                    .find(|t| t.name.eq_ignore_ascii_case(&source[index].name))
                    .map(|t| t.name.clone())
                    .or_else(|| targets.is_empty().then(|| source[index].name.clone()))
            } else {
                None
            };
        }
        changed = true;
    }
    let muted = theme::palette(ui.ctx()).text_muted;
    egui::Grid::new("transfer_mapping")
        .num_columns(5)
        .spacing([Spacing::L, Spacing::S])
        .striped(true)
        .show(ui, |ui| {
            for heading in ["Source", "Type", "Target column", "Convert to", "Transform"] {
                ui.label(RichText::new(heading).strong());
            }
            ui.end_row();
            for (i, column) in source.iter().enumerate() {
                let Some(m) = mapping.get_mut(i) else {
                    continue;
                };
                let mut included = m.target.is_some();
                if ui.checkbox(&mut included, column.name.as_str()).clicked() {
                    changed = true;
                    m.target = if !included {
                        None
                    } else if targets.is_empty() {
                        Some(column.name.clone())
                    } else {
                        targets
                            .iter()
                            .find(|t| t.name.eq_ignore_ascii_case(&column.name))
                            .map(|t| t.name.clone())
                            .or_else(|| targets.first().map(|t| t.name.clone()))
                    };
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&column.type_name).color(muted));
                    if let Some(target) =
                        targets.iter().find(|t| Some(&t.name) == m.target.as_ref())
                        && type_family(&column.type_name) != type_family(&target.type_name)
                    {
                        let p = theme::palette(ui.ctx());
                        theme::icons::paint(ui, theme::icons::WARNING, p.warning).on_hover_text(
                            format!(
                                "Type conversion: {} → {}. Preview values before running.",
                                column.type_name, target.type_name
                            ),
                        );
                    }
                });
                match &mut m.target {
                    Some(name) if targets.is_empty() => {
                        changed |= ui
                            .add(TextEdit::singleline(name).desired_width(140.0))
                            .changed();
                    }
                    Some(name) => {
                        ComboBox::from_id_salt(("transfer-target", i))
                            .selected_text(name.as_str())
                            .width(140.0)
                            .show_ui(ui, |ui| {
                                for t in targets {
                                    let picked =
                                        ui.selectable_label(*name == t.name, &t.name).clicked();
                                    if picked && *name != t.name {
                                        *name = t.name.clone();
                                        changed = true;
                                    }
                                }
                            });
                    }
                    None => {
                        ui.label(RichText::new("skipped").color(muted));
                    }
                }
                if m.target.is_some() {
                    let mut ty = m.target_type.clone().unwrap_or_default();
                    let hint = targets
                        .iter()
                        .find(|t| Some(&t.name) == m.target.as_ref())
                        .map_or("as the column", |t| t.type_name.as_str());
                    if ui
                        .add(
                            TextEdit::singleline(&mut ty)
                                .hint_text(hint)
                                .desired_width(110.0),
                        )
                        .changed()
                    {
                        m.target_type = (!ty.trim().is_empty()).then(|| ty.trim().to_string());
                        changed = true;
                    }
                    ui.horizontal(|ui| {
                        ComboBox::from_id_salt(("transfer-transform", i))
                            .selected_text(transform_label(&m.transform))
                            .show_ui(ui, |ui| {
                                for (label, make) in TRANSFORMS {
                                    let current = transform_label(&m.transform) == label;
                                    if ui.selectable_label(current, label).clicked() && !current {
                                        m.transform = make();
                                        changed = true;
                                    }
                                }
                            });
                        if let Some(Transform::Constant(text)) = &mut m.transform {
                            changed |= ui
                                .add(TextEdit::singleline(text).desired_width(90.0))
                                .changed();
                        }
                    });
                } else {
                    ui.label("");
                    ui.label("");
                }
                ui.end_row();
            }
        });
    changed
}

fn type_family(name: &str) -> &'static str {
    let name = name.to_ascii_lowercase();
    if [
        "int", "numeric", "decimal", "float", "double", "real", "number",
    ]
    .iter()
    .any(|n| name.contains(n))
    {
        "number"
    } else if ["char", "text", "string", "clob"]
        .iter()
        .any(|n| name.contains(n))
    {
        "text"
    } else if name.contains("date") || name.contains("time") {
        "time"
    } else if name.contains("bool") || name == "bit" {
        "boolean"
    } else if name.contains("binary") || name.contains("blob") || name.contains("bytea") {
        "binary"
    } else {
        "other"
    }
}
