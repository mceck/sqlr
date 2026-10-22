//! Statistics: the table statistics of a schema (sortable by any column, sizes in human
//! units), database sizes, and one table's statistics for the object editor.

use std::cmp::Ordering;

use egui::{ProgressBar, RichText, TextEdit};
use sqlr_core::TableStats;

use super::{ROW, caption, cell, row};
use crate::theme::{self, Spacing, icons};

/// `bytes` in binary units: "512 B", "1.5 KB", "20 MB" (one decimal under 10).
pub fn human_bytes(bytes: i64) -> String {
    if bytes.abs() < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    for unit in ["KB", "MB", "GB", "TB", "PB"] {
        value /= 1024.0;
        if value.abs() < 1024.0 || unit == "PB" {
            return if value.abs() < 10.0 {
                format!("{value:.1} {unit}")
            } else {
                format!("{value:.0} {unit}")
            };
        }
    }
    unreachable!("the last unit returns")
}

fn bytes_text(bytes: Option<i64>) -> String {
    bytes.map(human_bytes).unwrap_or_default()
}

/// A sortable column of the statistics table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatsColumn {
    Table,
    Rows,
    Total,
    Data,
    Indexes,
}

impl StatsColumn {
    const ALL: [StatsColumn; 5] = [
        StatsColumn::Table,
        StatsColumn::Rows,
        StatsColumn::Total,
        StatsColumn::Data,
        StatsColumn::Indexes,
    ];

    fn label(self) -> &'static str {
        match self {
            StatsColumn::Table => "Table",
            StatsColumn::Rows => "Rows",
            StatsColumn::Total => "Total size",
            StatsColumn::Data => "Data",
            StatsColumn::Indexes => "Indexes",
        }
    }

    fn width(self) -> f32 {
        match self {
            StatsColumn::Table => 260.0,
            _ => 110.0,
        }
    }

    fn number(self, s: &TableStats) -> Option<i64> {
        match self {
            StatsColumn::Table => None,
            StatsColumn::Rows => s.rows_estimate,
            StatsColumn::Total => s.total_bytes,
            StatsColumn::Data => s.table_bytes,
            StatsColumn::Indexes => s.index_bytes,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatsState {
    /// `(column, descending)`.
    pub sort: Option<(StatsColumn, bool)>,
    /// Case-insensitive substring of the table names.
    pub filter: String,
}

/// The order of `a` and `b` by `column`; numbers missing sort last either way.
fn compare(column: StatsColumn, desc: bool, a: &TableStats, b: &TableStats) -> Ordering {
    let ord = match column {
        StatsColumn::Table => a
            .table
            .name
            .to_lowercase()
            .cmp(&b.table.name.to_lowercase()),
        _ => match (column.number(a), column.number(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => return Ordering::Less,
            (None, Some(_)) => return Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
    };
    if desc { ord.reverse() } else { ord }
}

pub fn table_stats_view(ui: &mut egui::Ui, state: &mut StatsState, stats: &[TableStats]) {
    let filter = state.filter.to_lowercase();
    let mut shown: Vec<&TableStats> = stats
        .iter()
        .filter(|s| filter.is_empty() || s.table.name.to_lowercase().contains(&filter))
        .collect();
    if let Some((column, desc)) = state.sort {
        shown.sort_by(|a, b| compare(column, desc, a, b));
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        icons::paint(ui, icons::SEARCH, theme::palette(ui.ctx()).text_muted);
        ui.add(
            TextEdit::singleline(&mut state.filter)
                .hint_text("Filter tables")
                .desired_width(180.0),
        );
        let total: i64 = stats.iter().filter_map(|s| s.total_bytes).sum();
        caption(
            ui,
            &format!("{} tables · {}", stats.len(), human_bytes(total)),
        );
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        for column in StatsColumn::ALL {
            let sorted = state.sort.filter(|(c, _)| *c == column);
            let mut text = RichText::new(column.label()).strong();
            if sorted.is_some() {
                text = text.color(theme::palette(ui.ctx()).accent);
            }
            let header = ui.add_sized(
                [column.width(), ROW],
                egui::Button::new(text).frame_when_inactive(false),
            );
            if header.clicked() {
                state.sort = match sorted {
                    Some((_, false)) => Some((column, true)),
                    Some((_, true)) => None,
                    None => Some((column, column != StatsColumn::Table)),
                };
            }
            if let Some((_, desc)) = sorted {
                header.on_hover_text(if desc { "Descending" } else { "Ascending" });
            }
        }
        ui.label(RichText::new("Details").strong());
    });
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::both()
        .id_salt("table_stats")
        .auto_shrink([false, false])
        .show_rows(ui, ROW, shown.len(), |ui, range| {
            for s in &shown[range] {
                row(ui, None, |ui| {
                    cell(ui, StatsColumn::Table.width(), &s.table.name);
                    let rows = s.rows_estimate.map(|n| n.to_string()).unwrap_or_default();
                    cell(ui, StatsColumn::Rows.width(), rows);
                    cell(ui, StatsColumn::Total.width(), bytes_text(s.total_bytes));
                    cell(ui, StatsColumn::Data.width(), bytes_text(s.table_bytes));
                    cell(ui, StatsColumn::Indexes.width(), bytes_text(s.index_bytes));
                    let details: Vec<String> =
                        s.extra.iter().map(|(k, v)| format!("{k}: {v}")).collect();
                    let muted = theme::palette(ui.ctx()).text_muted;
                    ui.label(RichText::new(details.join(" · ")).color(muted));
                });
            }
        });
}

/// Each database's size, with a bar relative to the largest.
pub fn database_sizes_view(ui: &mut egui::Ui, sizes: &[(String, i64)]) {
    let largest = sizes.iter().map(|(_, b)| *b).max().unwrap_or(0).max(1);
    egui::ScrollArea::vertical()
        .id_salt("database_sizes")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("database_sizes_grid")
                .num_columns(3)
                .spacing([Spacing::L, Spacing::S])
                .striped(true)
                .show(ui, |ui| {
                    for (name, bytes) in sizes {
                        ui.label(name);
                        ui.label(human_bytes(*bytes));
                        ui.add(
                            ProgressBar::new(*bytes as f32 / largest as f32).desired_width(200.0),
                        );
                        ui.end_row();
                    }
                });
        });
}

/// One table's statistics as a form (the object editor's Properties).
pub fn table_stats_detail(ui: &mut egui::Ui, stats: &TableStats) {
    let muted = theme::palette(ui.ctx()).text_muted;
    egui::Grid::new("table_stats_detail")
        .num_columns(2)
        .spacing([Spacing::L, Spacing::S])
        .show(ui, |ui| {
            let line = |ui: &mut egui::Ui, key: &str, value: String| {
                ui.label(RichText::new(key).color(muted));
                ui.label(value);
                ui.end_row();
            };
            let rows = stats
                .rows_estimate
                .map(|n| n.to_string())
                .unwrap_or_default();
            line(ui, "Rows (estimate)", rows);
            line(ui, "Total size", bytes_text(stats.total_bytes));
            line(ui, "Data", bytes_text(stats.table_bytes));
            line(ui, "Indexes", bytes_text(stats.index_bytes));
            for (k, v) in &stats.extra {
                line(ui, k, v.clone());
            }
        });
}
