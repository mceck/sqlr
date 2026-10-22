//! The query history panel: search, a connection filter and a virtualized list of entries.
//!
//! The widget asks for entries through [`HistoryAction::Search`] and shows whatever the caller
//! passes back; it does no I/O.

use std::sync::OnceLock;

use egui::{ComboBox, RichText, TextEdit};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use sqlr_core::{HistoryEntry, HistoryOutcome, HistoryQuery};

use crate::first_line;
use crate::theme::{self, icons};

/// Entries asked for per search.
const SEARCH_LIMIT: usize = 200;
/// Chars of the first SQL line shown in a row.
const LINE_CHARS: usize = 200;

#[derive(Default)]
pub struct HistoryPanelState {
    pub search: String,
    /// The connection id to show; `None` shows every connection.
    pub conn_filter: Option<String>,
    selected: Option<i64>,
    /// The `(search, conn_filter)` of the last emitted `Search`; `None` before the first.
    issued: Option<(String, Option<String>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HistoryAction {
    Search(HistoryQuery),
    Open(HistoryEntry),
    Insert(HistoryEntry),
    /// Put the entry's SQL on the clipboard.
    Copy(HistoryEntry),
    /// Delete stored history, for one connection or (`None`) every connection.
    Clear {
        conn_id: Option<String>,
    },
}

/// `conns` are `(id, name)` pairs; `entries` is `None` while the search runs.
pub fn history_panel(
    ui: &mut egui::Ui,
    state: &mut HistoryPanelState,
    conns: &[(String, String)],
    entries: Option<&[HistoryEntry]>,
) -> Vec<HistoryAction> {
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut state.search)
                .hint_text("Search history")
                .desired_width(320.0),
        );
        let current = state
            .conn_filter
            .as_ref()
            .and_then(|id| conns.iter().find(|(i, _)| i == id))
            .map_or("All connections", |(_, name)| name.as_str());
        if icons::icon_button(ui, icons::DELETE, None, "Clear history").clicked() {
            actions.push(HistoryAction::Clear {
                conn_id: state.conn_filter.clone(),
            });
        }
        ComboBox::from_id_salt("history_conn_filter")
            .selected_text(current)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(state.conn_filter.is_none(), "All connections")
                    .clicked()
                {
                    state.conn_filter = None;
                }
                for (id, name) in conns {
                    let selected = state.conn_filter.as_ref() == Some(id);
                    if ui.selectable_label(selected, name).clicked() {
                        state.conn_filter = Some(id.clone());
                    }
                }
            });
    });
    let wanted = (state.search.clone(), state.conn_filter.clone());
    if state.issued.as_ref() != Some(&wanted) {
        actions.push(HistoryAction::Search(HistoryQuery {
            text: wanted.0.clone(),
            conn_id: wanted.1.clone(),
            limit: SEARCH_LIMIT,
        }));
        state.issued = Some(wanted);
    }
    ui.add_space(2.0);

    let muted = theme::palette(ui.ctx()).text_muted;
    let Some(entries) = entries else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Loading…").color(muted));
        });
        return actions;
    };
    if entries.is_empty() {
        ui.label(RichText::new("No history").color(muted));
        return actions;
    }
    let row_height = ui.spacing().interact_size.y;
    egui::ScrollArea::vertical()
        .id_salt("history_rows")
        .auto_shrink(false)
        .show_rows(ui, row_height, entries.len(), |ui, range| {
            for entry in &entries[range] {
                entry_row(ui, state, conns, entry, &mut actions);
            }
        });
    actions
}

fn entry_row(
    ui: &mut egui::Ui,
    state: &mut HistoryPanelState,
    conns: &[(String, String)],
    entry: &HistoryEntry,
    actions: &mut Vec<HistoryAction>,
) {
    let palette = theme::palette(ui.ctx());
    let muted = palette.text_muted;
    ui.horizontal(|ui| {
        ui.label(RichText::new(format_when(entry.started_at_ms)).color(muted));
        let conn = conns
            .iter()
            .find(|(id, _)| *id == entry.conn_id)
            .map_or(entry.conn_id.as_str(), |(_, name)| name.as_str());
        ui.label(conn);
        ui.label(RichText::new(format_duration(entry.duration_ms)).color(muted));
        let (icon, icon_color, badge, color) = match &entry.outcome {
            HistoryOutcome::Rows(n) => {
                (icons::SUCCESS, palette.success, format!("{n} rows"), muted)
            }
            HistoryOutcome::Affected(n) => (
                icons::AFFECTED,
                palette.success,
                format!("{n} affected"),
                muted,
            ),
            HistoryOutcome::Failed(_) => (
                icons::FAILED,
                palette.error,
                "failed".to_string(),
                palette.error,
            ),
            HistoryOutcome::Cancelled => (icons::CANCELLED, muted, "cancelled".to_string(), muted),
        };
        icons::paint(ui, icon, icon_color);
        let badge = ui.label(RichText::new(badge).small().color(color));
        if let HistoryOutcome::Failed(message) = &entry.outcome {
            badge.on_hover_text(message);
        }
        let selected = state.selected == Some(entry.id);
        let response = ui
            .selectable_label(selected, first_line(&entry.sql, LINE_CHARS))
            .on_hover_text(&entry.sql);
        if response.clicked() {
            state.selected = Some(entry.id);
        }
        if response.double_clicked() {
            actions.push(HistoryAction::Open(entry.clone()));
        }
        theme::context_menu(&response, |ui| {
            if icons::menu_item(ui, icons::OPEN, "Open in new tab").clicked() {
                actions.push(HistoryAction::Open(entry.clone()));
                ui.close();
            }
            if icons::menu_item(ui, icons::INSERT, "Insert into editor").clicked() {
                actions.push(HistoryAction::Insert(entry.clone()));
                ui.close();
            }
            if icons::menu_item(ui, icons::COPY, "Copy SQL").clicked() {
                actions.push(HistoryAction::Copy(entry.clone()));
                ui.close();
            }
        });
    });
}

/// Unix milliseconds as local time, `YYYY-MM-DD HH:MM:SS`.
pub fn format_when(started_at_ms: i64) -> String {
    // Looked up once: the system zone is read from disk.
    static ZONE: OnceLock<TimeZone> = OnceLock::new();
    let zone = ZONE.get_or_init(TimeZone::system);
    match Timestamp::from_millisecond(started_at_ms) {
        Ok(ts) => ts
            .to_zoned(zone.clone())
            .strftime("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => String::new(),
    }
}

/// `850 ms`, `1.2 s`, `2 m 05 s`.
pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{}.{} s", ms / 1000, (ms % 1000) / 100)
    } else {
        format!("{} m {:02} s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_boundaries() {
        assert_eq!(format_duration(0), "0 ms");
        assert_eq!(format_duration(999), "999 ms");
        assert_eq!(format_duration(1000), "1.0 s");
        assert_eq!(format_duration(59_999), "59.9 s");
        assert_eq!(format_duration(60_000), "1 m 00 s");
    }
}
