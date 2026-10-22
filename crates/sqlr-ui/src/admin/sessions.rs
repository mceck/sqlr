//! The sessions table: filters (hide idle, user, database), the refresh controls (refresh
//! now, pause, interval) and the selection's actions (cancel its query, terminate it). The
//! app's own sessions are marked "this connection" and tinted. The caller refreshes on the
//! interval while the table is shown, and asks before an action.

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{ComboBox, DragValue, RichText};
use sqlr_core::SessionInfo;

use super::{ROW, caption, cell, heading, row};
use crate::first_line;
use crate::theme::{self, Spacing, icons};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionsState {
    pub hide_idle: bool,
    /// Only this user's sessions.
    pub user: Option<String>,
    /// Only sessions on this database.
    pub database: Option<String>,
    /// The selected session's id.
    pub selected: Option<String>,
    /// Auto-refresh is paused.
    pub paused: bool,
    /// Seconds between refreshes (1–60).
    pub interval_s: u32,
    /// Why Cancel query and Terminate are off (a read-only session); set by the caller.
    pub actions_blocked: Option<String>,
}

impl Default for SessionsState {
    fn default() -> Self {
        Self {
            hide_idle: false,
            user: None,
            database: None,
            selected: None,
            paused: false,
            interval_s: 2,
            actions_blocked: None,
        }
    }
}

impl SessionsState {
    /// The sessions the filters let through, in order.
    pub fn visible<'a>(&self, sessions: &'a [SessionInfo]) -> Vec<&'a SessionInfo> {
        sessions
            .iter()
            .filter(|s| !(self.hide_idle && s.is_idle()))
            .filter(|s| self.user.as_ref().is_none_or(|u| s.user == *u))
            .filter(|s| {
                self.database
                    .as_ref()
                    .is_none_or(|d| s.database.as_deref() == Some(d.as_str()))
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionsAction {
    /// Refresh now.
    Refresh,
    /// Cancel the running query of the session with this id.
    Cancel(String),
    /// End the session with this id.
    Terminate(String),
}

const W_ID: f32 = 70.0;
const W_OWN: f32 = 110.0;
const W_USER: f32 = 110.0;
const W_DB: f32 = 110.0;
const W_CLIENT: f32 = 160.0;
const W_STATE: f32 = 130.0;
const W_WAIT: f32 = 160.0;
const W_TIME: f32 = 70.0;
const W_QUERY: f32 = 420.0;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// `ms` as "850 ms", "12 s", "3 min", "2 h".
fn elapsed(ms: i64) -> String {
    let ms = ms.max(0);
    match ms {
        0..1_000 => format!("{ms} ms"),
        1_000..60_000 => format!("{} s", ms / 1_000),
        60_000..3_600_000 => format!("{} min", ms / 60_000),
        _ => format!("{} h", ms / 3_600_000),
    }
}

/// A filter combo over `values`: "All …" or one of them.
fn filter_combo(
    ui: &mut egui::Ui,
    id: &str,
    all: &str,
    value: &mut Option<String>,
    values: &BTreeSet<String>,
) {
    let text = value.clone().unwrap_or_else(|| all.to_string());
    ComboBox::from_id_salt(id)
        .selected_text(text)
        .width(120.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, all);
            for v in values {
                ui.selectable_value(value, Some(v.clone()), v);
            }
        });
}

pub fn sessions_table(
    ui: &mut egui::Ui,
    state: &mut SessionsState,
    sessions: &[SessionInfo],
) -> Option<SessionsAction> {
    let mut action = None;
    let p = theme::palette(ui.ctx());
    let users: BTreeSet<String> = sessions.iter().map(|s| s.user.clone()).collect();
    let databases: BTreeSet<String> = sessions.iter().filter_map(|s| s.database.clone()).collect();
    let visible = state.visible(sessions);
    let selected = state
        .selected
        .as_ref()
        .filter(|id| visible.iter().any(|s| s.id == **id))
        .cloned();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        if icons::button(ui, true, icons::REFRESH, None, "Refresh").clicked() {
            action = Some(SessionsAction::Refresh);
        }
        let (icon, label) = if state.paused {
            (icons::RUN, "Resume")
        } else {
            (icons::TIMER, "Pause")
        };
        if icons::button(ui, true, icon, None, label)
            .on_hover_text("Automatic refresh")
            .clicked()
        {
            state.paused = !state.paused;
        }
        caption(ui, "every");
        ui.add(
            DragValue::new(&mut state.interval_s)
                .range(1..=60)
                .suffix(" s"),
        );
        ui.separator();
        ui.checkbox(&mut state.hide_idle, "Hide idle");
        filter_combo(ui, "sessions_user", "All users", &mut state.user, &users);
        filter_combo(
            ui,
            "sessions_database",
            "All databases",
            &mut state.database,
            &databases,
        );
        ui.separator();
        let id = selected.clone();
        let has = id.is_some() && state.actions_blocked.is_none();
        let blocked = state.actions_blocked.clone().unwrap_or_default();
        let cancel = icons::button(ui, has, icons::STOP, Some(p.warning), "Cancel query")
            .on_hover_text("Cancel the selected session's running query")
            .on_disabled_hover_text(&blocked);
        if cancel.clicked()
            && let Some(id) = &id
        {
            action = Some(SessionsAction::Cancel(id.clone()));
        }
        let terminate = icons::button(ui, has, icons::CLOSE, Some(p.error), "Terminate")
            .on_hover_text("End the selected session")
            .on_disabled_hover_text(&blocked);
        if terminate.clicked()
            && let Some(id) = &id
        {
            action = Some(SessionsAction::Terminate(id.clone()));
        }
        let count = if visible.len() == sessions.len() {
            format!("{} sessions", sessions.len())
        } else {
            format!("{} of {} sessions", visible.len(), sessions.len())
        };
        caption(ui, &count);
    });
    ui.add_space(Spacing::S);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        heading(ui, W_ID, "Id");
        heading(ui, W_OWN, "");
        heading(ui, W_USER, "User");
        heading(ui, W_DB, "Database");
        heading(ui, W_CLIENT, "Client");
        heading(ui, W_STATE, "State");
        heading(ui, W_WAIT, "Wait");
        heading(ui, W_TIME, "Running");
        heading(ui, W_QUERY, "Query");
    });
    let now = now_ms();
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::both()
        .id_salt("sessions_table")
        .auto_shrink([false, false])
        .show_rows(ui, ROW, visible.len(), |ui, range| {
            for s in &visible[range] {
                let is_selected = selected.as_deref() == Some(s.id.as_str());
                let fill = if is_selected {
                    Some(p.selection)
                } else if s.own {
                    Some(p.accent_soft)
                } else {
                    None
                };
                row(ui, fill, |ui| {
                    let id = ui.allocate_ui_with_layout(
                        egui::vec2(W_ID, ROW),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_width(W_ID);
                            ui.selectable_label(is_selected, &s.id)
                        },
                    );
                    if id.inner.clicked() {
                        state.selected = Some(s.id.clone());
                    }
                    if s.own {
                        cell(ui, W_OWN, RichText::new("this connection").color(p.accent))
                            .on_hover_text("One of sqlr's own sessions on this connection");
                    } else {
                        cell(ui, W_OWN, "");
                    }
                    cell(ui, W_USER, &s.user);
                    cell(ui, W_DB, s.database.as_deref().unwrap_or(""));
                    cell(ui, W_CLIENT, s.client.as_deref().unwrap_or(""));
                    cell(ui, W_STATE, &s.state);
                    cell(ui, W_WAIT, s.wait.as_deref().unwrap_or(""));
                    let running = s
                        .query_started_ms
                        .filter(|_| !s.is_idle())
                        .map(|at| elapsed(now - at))
                        .unwrap_or_default();
                    cell(ui, W_TIME, running);
                    let query = first_line(s.query.as_deref().unwrap_or(""), usize::MAX);
                    let q = cell(ui, W_QUERY, RichText::new(query).monospace());
                    if q.clicked() {
                        state.selected = Some(s.id.clone());
                    }
                });
            }
        });
    action
}

#[cfg(test)]
mod tests {
    use super::elapsed;

    #[test]
    fn elapsed_times_are_short() {
        assert_eq!(elapsed(850), "850 ms");
        assert_eq!(elapsed(12_400), "12 s");
        assert_eq!(elapsed(185_000), "3 min");
        assert_eq!(elapsed(7_300_000), "2 h");
        assert_eq!(elapsed(-5), "0 ms");
    }
}
