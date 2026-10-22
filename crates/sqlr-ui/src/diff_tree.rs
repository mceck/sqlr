//! The schema compare views (spec §8.6): the tree of differing objects with a status filter,
//! a name filter and an include checkbox per object, and the side-by-side text diff of an
//! object's DDL.

use std::collections::HashSet;

use egui::{Color32, Id, RichText, TextEdit, WidgetInfo, WidgetType};
use similar::{DiffOp, TextDiff};
use sqlr_core::{DiffKind, ObjectDiff, ObjectKind, ObjectRef};

use crate::theme::{self, Spacing};

/// Which differences the tree lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Filter {
    status: Option<DiffKind>,
    /// Only views (and materialized views).
    views_only: bool,
    text: String,
}

impl Filter {
    fn keeps(&self, d: &ObjectDiff) -> bool {
        if self.status.is_some_and(|s| s != d.kind) {
            return false;
        }
        if self.views_only
            && !matches!(
                d.object.kind,
                ObjectKind::View | ObjectKind::MaterializedView
            )
        {
            return false;
        }
        self.text.trim().is_empty()
            || d.object
                .name
                .to_lowercase()
                .contains(&self.text.trim().to_lowercase())
    }
}

fn sign(kind: DiffKind) -> &'static str {
    match kind {
        DiffKind::Added => "+",
        DiffKind::Removed => "−",
        DiffKind::Changed => "~",
    }
}

/// Draws the differences: filters on top, then one row per object (virtualized: a schema of
/// thousands of tables stays smooth). Clicking a name sets `selected` (an index into
/// `diffs`); unchecking an object adds it to `excluded`.
pub fn diff_tree(
    ui: &mut egui::Ui,
    diffs: &[ObjectDiff],
    excluded: &mut HashSet<ObjectRef>,
    selected: &mut Option<usize>,
) {
    let p = theme::palette(ui.ctx());
    let id = Id::new("diff-tree-filter");
    let mut filter: Filter = ui.data_mut(|d| d.get_temp(id)).unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        for (label, status) in [
            ("All", None),
            ("Added", Some(DiffKind::Added)),
            ("Removed", Some(DiffKind::Removed)),
            ("Changed", Some(DiffKind::Changed)),
        ] {
            if ui
                .selectable_label(!filter.views_only && filter.status == status, label)
                .clicked()
            {
                filter.status = status;
                filter.views_only = false;
            }
        }
        if ui.selectable_label(filter.views_only, "Views").clicked() {
            filter.views_only = !filter.views_only;
        }
    });
    ui.add(
        TextEdit::singleline(&mut filter.text)
            .hint_text("Filter by name")
            .desired_width(f32::INFINITY),
    );
    ui.data_mut(|d| d.insert_temp(id, filter.clone()));
    let visible: Vec<usize> = (0..diffs.len())
        .filter(|&i| filter.keeps(&diffs[i]))
        .collect();
    let counts = format!(
        "{} of {} objects{}",
        visible.len(),
        diffs.len(),
        if excluded.is_empty() {
            String::new()
        } else {
            format!(" · {} excluded", excluded.len())
        }
    );
    ui.label(RichText::new(counts).small().color(p.text_muted));
    theme::hairline(ui);
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + Spacing::M;
    egui::ScrollArea::vertical()
        .id_salt("diff-tree-rows")
        .auto_shrink([false, false])
        .show_rows(ui, row_height, visible.len(), |ui, range| {
            for &i in &visible[range] {
                let d = &diffs[i];
                ui.horizontal(|ui| {
                    let mut included = !excluded.contains(&d.object);
                    let name = d.object.name.clone();
                    let check = ui.add(egui::Checkbox::without_text(&mut included));
                    check.widget_info(|| {
                        WidgetInfo::selected(
                            WidgetType::Checkbox,
                            true,
                            included,
                            format!("Include {name}"),
                        )
                    });
                    if check.clicked() {
                        if included {
                            excluded.remove(&d.object);
                        } else {
                            excluded.insert(d.object.clone());
                        }
                    }
                    let color = match d.kind {
                        DiffKind::Added => p.success,
                        DiffKind::Removed => p.error,
                        DiffKind::Changed => p.warning,
                    };
                    ui.label(RichText::new(sign(d.kind)).color(color).monospace());
                    ui.label(
                        RichText::new(d.object.kind.keyword().to_lowercase())
                            .small()
                            .color(p.text_muted),
                    );
                    let is_selected = *selected == Some(i);
                    if ui.selectable_label(is_selected, &d.object.name).clicked() {
                        *selected = Some(i);
                    }
                    if d.destructive {
                        let warn = ui.label(RichText::new("⚠").color(p.warning));
                        let name = d.object.name.clone();
                        warn.widget_info(|| {
                            WidgetInfo::labeled(
                                WidgetType::Label,
                                true,
                                format!("Destructive: {name}"),
                            )
                        });
                    }
                });
            }
        });
}

/// How a row of a side-by-side diff reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRowKind {
    Same,
    /// Both sides have a line, and they differ.
    Changed,
    /// Only the right side has the line.
    Added,
    /// Only the left side has the line.
    Removed,
}

/// One row: the left and right lines with their 1-based numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub left: Option<(usize, String)>,
    pub right: Option<(usize, String)>,
    pub kind: DiffRowKind,
}

/// `left` against `right` line by line, a replaced block's lines paired up.
pub fn diff_rows(left: &str, right: &str) -> Vec<DiffRow> {
    let old: Vec<&str> = left.lines().collect();
    let new: Vec<&str> = right.lines().collect();
    let diff = TextDiff::from_lines(left, right);
    let line = |lines: &[&str], i: usize| (i + 1, lines.get(i).copied().unwrap_or("").to_string());
    let mut rows = Vec::new();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for k in 0..len {
                    rows.push(DiffRow {
                        left: Some(line(&old, old_index + k)),
                        right: Some(line(&new, new_index + k)),
                        kind: DiffRowKind::Same,
                    });
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for k in 0..old_len {
                    rows.push(DiffRow {
                        left: Some(line(&old, old_index + k)),
                        right: None,
                        kind: DiffRowKind::Removed,
                    });
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for k in 0..new_len {
                    rows.push(DiffRow {
                        left: None,
                        right: Some(line(&new, new_index + k)),
                        kind: DiffRowKind::Added,
                    });
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                for k in 0..old_len.max(new_len) {
                    let l = (k < old_len).then(|| line(&old, old_index + k));
                    let r = (k < new_len).then(|| line(&new, new_index + k));
                    let kind = match (&l, &r) {
                        (Some(_), Some(_)) => DiffRowKind::Changed,
                        (Some(_), None) => DiffRowKind::Removed,
                        _ => DiffRowKind::Added,
                    };
                    rows.push(DiffRow {
                        left: l,
                        right: r,
                        kind,
                    });
                }
            }
        }
    }
    rows
}

/// Draws `left` and `right` side by side with line numbers; changed lines are tinted.
pub fn text_diff(ui: &mut egui::Ui, left: &str, right: &str) {
    let p = theme::palette(ui.ctx());
    let rows = diff_rows(left, right);
    let tint = |kind: DiffRowKind, left_side: bool| -> Color32 {
        match (kind, left_side) {
            (DiffRowKind::Same, _) => Color32::TRANSPARENT,
            (DiffRowKind::Removed, true) | (DiffRowKind::Changed, true) => {
                p.error.gamma_multiply(0.18)
            }
            (DiffRowKind::Added, false) | (DiffRowKind::Changed, false) => {
                p.success.gamma_multiply(0.18)
            }
            _ => Color32::TRANSPARENT,
        }
    };
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
    egui::ScrollArea::both()
        .id_salt("text-diff")
        .auto_shrink([false, false])
        .show_rows(ui, row_height, rows.len(), |ui, range| {
            for row in &rows[range] {
                ui.horizontal(|ui| {
                    for (side, cell) in [(true, &row.left), (false, &row.right)] {
                        let width = if side {
                            (ui.available_width() / 2.0).max(120.0)
                        } else {
                            ui.available_width().max(120.0)
                        };
                        let text = match cell {
                            Some((n, line)) => format!("{n:>4}  {line}"),
                            None => String::new(),
                        };
                        ui.add_sized(
                            [width, row_height],
                            egui::Label::new(
                                RichText::new(text)
                                    .monospace()
                                    .background_color(tint(row.kind, side)),
                            )
                            .truncate(),
                        );
                    }
                });
            }
        });
}
