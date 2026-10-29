//! The bottom status bar: the tab's connection state, its transaction or commit mode, row
//! count and timing, the "Fetch: N" selector and "Fetch all", a running export or copy with
//! its Cancel, a warning while history and snippets are kept in memory only, and the
//! caret's line and column.

use std::time::Duration;

use egui::{Button, ComboBox, DragValue, RichText, Sense};
use sqlr_engine::{Engine, QueryId, QueryStatus, SessionStatus};
use sqlr_store::PAGE_SIZES;
use sqlr_ui::status_segment;
use sqlr_ui::theme::{self, Palette, Spacing, icons};

use crate::transaction::TxLine;

const PRESETS: [usize; 4] = [100, 500, 1000, 5000];
/// Appended to a result whose stream ended before its last row, so its row count is not
/// mistaken for the query's total.
const CUT_SHORT: &str =
    " · stopped early: another query ran on this connection or it closed — re-run to continue";

/// What the status bar shows of one result, copied out of the result lock.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultInfo {
    pub status: QueryStatus,
    pub rows: usize,
    pub affected: Option<u64>,
    pub elapsed: Duration,
    pub cut_short: bool,
}

impl ResultInfo {
    /// Reads the result and releases its lock before returning.
    pub fn read(engine: &Engine, q: QueryId) -> Option<Self> {
        let result = engine.result(q)?;
        let r = result.read();
        Some(Self {
            status: r.status.clone(),
            rows: r.rows.row_count(),
            affected: r.affected,
            elapsed: r.elapsed,
            cut_short: r.cut_short,
        })
    }

    pub fn more_available(&self) -> bool {
        self.status == QueryStatus::Ready { more: true }
    }

    /// The status bar's two segments: what came back, and the time (or the run's state).
    pub fn parts(&self) -> (String, String) {
        let ms = self.elapsed.as_millis();
        match (&self.status, self.affected) {
            (QueryStatus::Failed(_), _) => ("Failed".into(), format!("{ms} ms")),
            (QueryStatus::Running, _) => (format!("{} rows", self.rows), "running…".into()),
            (QueryStatus::Cancelled, _) => (format!("{} rows", self.rows), "cancelled".into()),
            (QueryStatus::Ready { .. }, Some(_)) => ("Done".into(), format!("{ms} ms")),
            (QueryStatus::Ready { more }, None) => {
                let more = if *more { " (more available)" } else { "" };
                let cut = if self.cut_short { CUT_SHORT } else { "" };
                (format!("{} rows{more}", self.rows), format!("{ms} ms{cut}"))
            }
        }
    }
}

/// The active tab's connection, for the status bar's state dot.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnInfo {
    pub name: String,
    /// The driver's name.
    pub driver: String,
    /// `None` when the connection has no session.
    pub status: Option<SessionStatus>,
}

impl ConnInfo {
    fn state(&self, p: &Palette) -> (egui::Color32, &'static str) {
        match &self.status {
            Some(SessionStatus::Connected(_)) => (p.success, "Connected"),
            Some(SessionStatus::Connecting) => (p.warning, "Connecting…"),
            Some(SessionStatus::Failed(_)) => (p.error, "Connection failed"),
            Some(SessionStatus::Disconnected) | None => (p.text_disabled, "Offline"),
        }
    }
}

/// What the status bar shows this frame.
#[derive(Default)]
pub struct StatusView<'a> {
    pub conn: Option<ConnInfo>,
    /// The active tab's open transaction.
    pub transaction: Option<TxLine>,
    /// The active editor's caret, 1-based line and column.
    pub caret: Option<(usize, usize)>,
    /// The active tab's page size; `None` without a tab.
    pub page_size: Option<usize>,
    pub info: Option<&'a ResultInfo>,
    pub selection: Option<&'a str>,
    /// Why history and snippets will not be saved, while that holds.
    pub storage_warning: Option<&'a str>,
    /// A running export or copy: its progress line.
    pub job: Option<&'a str>,
    /// The MCP server's state, including when it is disabled.
    pub mcp: Option<McpView>,
    /// Whether the terminal panel is open.
    pub terminal_open: bool,
}

/// What the status bar shows of the MCP server.
#[derive(Clone, Debug, PartialEq)]
pub struct McpView {
    pub enabled: bool,
    pub running: bool,
    pub sessions: usize,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StatusAction {
    SetPageSize(usize),
    FetchAll,
    /// Cancel the running job.
    CancelJob,
    OpenAgents,
    ToggleMcp,
    /// Open the command palette.
    OpenCommands,
    ToggleTerminal,
}

#[derive(Default)]
pub struct StatusBarState {
    /// The value being edited in the "Custom…" popup, while it is open.
    custom: Option<usize>,
}

/// The widest a text segment of the left group grows before its ellipsis.
const SEGMENT_MAX: f32 = 260.0;

/// Room for a control of about `width` points: a control that does not fit is left out
/// rather than drawn past the bar's right group.
fn fits(ui: &egui::Ui, width: f32) -> bool {
    ui.available_width() >= width
}

/// A text segment cut with an ellipsis at [`SEGMENT_MAX`] or the room left after
/// `reserve` points.
fn segment_leaving(ui: &mut egui::Ui, text: &str, color: Option<egui::Color32>, reserve: f32) {
    let width = (ui.available_width() - reserve).min(SEGMENT_MAX);
    if width < 24.0 {
        return;
    }
    ui.scope(|ui| {
        ui.set_max_width(width);
        status_segment(ui, text, color);
    });
}

/// The connection, transaction, result, fetch and job segments, in priority order: each
/// one is shown only while there is room left for it.
fn left_group(
    ui: &mut egui::Ui,
    state: &mut StatusBarState,
    view: &StatusView<'_>,
    palette: &Palette,
    action: &mut Option<StatusAction>,
) {
    // Priority: the state word, the row count, the transaction line, then the connection's
    // name. The earlier segments leave room for the more important later ones.
    let reserve = if view.info.is_some() { 130.0 } else { 0.0 };
    let tx_reserve = reserve
        + if view.transaction.is_some() {
            140.0
        } else {
            0.0
        };
    let segment = |ui: &mut egui::Ui, text: &str, color| segment_leaving(ui, text, color, 0.0);
    let squeezed = |ui: &mut egui::Ui, text: &str, color| segment_leaving(ui, text, color, reserve);
    if let Some(conn) = &view.conn {
        let (color, word) = conn.state(palette);
        let (rect, dot) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        dot.on_hover_text(format!("{}: {word}", conn.name));
        segment(ui, word, Some(palette.text));
        segment_leaving(
            ui,
            &format!("· {} · {}", conn.name, conn.driver),
            None,
            tx_reserve,
        );
    }
    if let Some(tx) = &view.transaction
        && fits(ui, 60.0 + reserve)
    {
        let color = if tx.failed {
            palette.error
        } else {
            palette.warning
        };
        ui.separator();
        squeezed(ui, &tx.text, Some(color));
    }
    if let Some(info) = view.info
        && fits(ui, 120.0)
    {
        let (rows, time) = info.parts();
        ui.separator();
        icons::paint(ui, icons::ROWS, palette.text_muted);
        segment(ui, &rows, Some(palette.text));
        if fits(ui, 60.0) {
            icons::paint(ui, icons::TIMER, palette.text_muted);
            segment(ui, &time, Some(palette.text));
        }
    }
    if let Some(selection) = view.selection
        && fits(ui, 120.0)
    {
        ui.separator();
        segment(ui, selection, Some(palette.text));
    }
    // Paging only means something for a result that returned rows.
    let rows_result = view
        .info
        .is_some_and(|i| i.affected.is_none() && !matches!(i.status, QueryStatus::Failed(_)));
    if let Some(page_size) = view.page_size.filter(|_| rows_result)
        && fits(ui, 180.0)
    {
        ui.separator();
        ComboBox::from_id_salt("fetch_page_size")
            .selected_text(format!("Fetch {page_size} rows"))
            .show_ui(ui, |ui| {
                for n in PRESETS {
                    if ui.selectable_label(page_size == n, n.to_string()).clicked() {
                        *action = Some(StatusAction::SetPageSize(n));
                    }
                }
                let custom = !PRESETS.contains(&page_size);
                if ui.selectable_label(custom, "Custom…").clicked() {
                    state.custom = Some(page_size);
                }
            })
            .response
            .on_hover_text(
                "Rows loaded at a time; scrolling to the end loads the next page. \
                 Sorting and filtering re-run the query on the server.",
            );
        if view.info.is_some_and(ResultInfo::more_available) {
            let all = ui
                .button("Load all rows")
                .on_hover_text(format!("Only the first {page_size} rows are loaded so far"));
            if all.clicked() {
                *action = Some(StatusAction::FetchAll);
            }
        }
    }
    if let Some(job) = view.job
        && fits(ui, 140.0)
    {
        ui.separator();
        ui.spinner();
        segment(ui, job, None);
        if fits(ui, 70.0) && ui.button("Cancel job").clicked() {
            *action = Some(StatusAction::CancelJob);
        }
    }
}

pub fn status_bar(
    ui: &mut egui::Ui,
    state: &mut StatusBarState,
    view: &StatusView<'_>,
) -> Option<StatusAction> {
    let mut action = None;
    let palette = theme::palette(ui.ctx());
    // The right group first, so the left one gets what is left and never runs under it.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        // The palette's shortcut, in view so it is learned; the first to go in a narrow window.
        if let Some(keys) = sqlr_ui::keymap::shortcut(ui.ctx(), sqlr_core::ActionId::Commands)
            .filter(|_| fits(ui, 900.0))
        {
            let response = ui
                .add(
                    Button::new(
                        RichText::new(format!("{keys}  Commands"))
                            .small()
                            .color(palette.text_muted),
                    )
                    .frame(false),
                )
                .on_hover_text("Open the command palette");
            if response.clicked() {
                action = Some(StatusAction::OpenCommands);
            }
            ui.separator();
        }
        if let Some(mcp) = view.mcp.as_ref().filter(|_| fits(ui, 90.0)) {
            let color = if mcp.enabled && mcp.running {
                palette.success
            } else {
                palette.text_disabled
            };
            let response = ui
                .add(
                    Button::new(RichText::new(format!("MCP {}", mcp.sessions)).small())
                        .frame(false),
                )
                .on_hover_text(&mcp.detail);
            if response.clicked() {
                action = Some(StatusAction::OpenAgents);
            }
            // Allocate the dot's own hit area so its paint stays inside its segment.
            let (rect, toggle) = ui.allocate_exact_size(egui::vec2(16.0, 20.0), Sense::click());
            ui.painter().circle_filled(rect.center(), 3.0, color);
            toggle.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button, true, "Toggle agent access (MCP)",
                )
            });
            let detail = if mcp.enabled {
                "AI assistants on this computer can query the connections you chose. Click to turn it off."
            } else {
                "Let AI assistants on this computer query the connections you choose. Click to turn it on."
            };
            if toggle.on_hover_text(detail).clicked() {
                action = Some(StatusAction::ToggleMcp);
            }
            ui.separator();
        }
        // Right to left: the label, then its icon before it.
        let color = if view.terminal_open {
            palette.text
        } else {
            palette.text_muted
        };
        let compact = ui.max_rect().width() < 700.0;
        let terminal = if compact {
            let response = ui.add(
                Button::new(RichText::new(icons::TERMINAL)
                    .font(sqlr_ui::theme::fonts::icons(ui.ctx(), icons::SIZE))
                    .color(color))
                    .frame(false)
                    .selected(view.terminal_open),
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Terminal")
            });
            response
        } else {
            let response = ui.add(
                Button::new(RichText::new("Terminal").small().color(color))
                    .frame(false)
                    .selected(view.terminal_open),
            );
            icons::paint(ui, icons::TERMINAL, color);
            response
        };
        let terminal = terminal.on_hover_text(
            match sqlr_ui::keymap::shortcut(ui.ctx(), sqlr_core::ActionId::ToggleTerminal) {
                Some(keys) => format!("Show or hide the terminal ({keys})"),
                None => "Show or hide the terminal".to_string(),
            },
        );
        if terminal.clicked() {
            action = Some(StatusAction::ToggleTerminal);
        }
        // The editor's segments, only while an editor is in front.
        if let Some((line, col)) = view.caret {
            ui.separator();
            status_segment(ui, "UTF-8", None);
            ui.separator();
            status_segment(ui, &format!("Ln {line}, Col {col}"), None);
        }
        if let Some(reason) = view.storage_warning {
            ui.separator();
            ui.label(RichText::new("Not saved").color(palette.warning))
                .on_hover_text(reason);
            icons::paint(ui, icons::WARNING, palette.warning);
        }
        ui.separator();
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = Spacing::S;
            left_group(ui, state, view, &palette, &mut action);
        });
    });
    if let Some(value) = &mut state.custom {
        let response = theme::Dialog::new("custom_fetch_size", icons::ROWS, "Custom fetch size")
            .subtitle("Choose how many rows to load in each result page")
            .width(420.0)
            .show(ui.ctx(), |d| {
                d.body(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Rows per fetch");
                        ui.add(DragValue::new(value).range(PAGE_SIZES));
                    });
                });
                d.footer(|f| {
                    let apply = f.primary("Apply", None);
                    f.cancel();
                    apply
                })
            });
        if response.inner {
            action = Some(StatusAction::SetPageSize(*value)).or(action);
            state.custom = None;
        } else if response.closed {
            state.custom = None;
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::{DriverError, ErrorKind};

    impl ResultInfo {
        fn summary(&self) -> String {
            let (rows, time) = self.parts();
            format!("{rows} · {time}")
        }
    }

    fn info(status: QueryStatus, rows: usize, affected: Option<u64>) -> ResultInfo {
        ResultInfo {
            status,
            rows,
            affected,
            elapsed: Duration::from_millis(42),
            cut_short: false,
        }
    }

    #[test]
    fn summary_per_status() {
        let ready = |more| QueryStatus::Ready { more };
        assert_eq!(
            info(ready(true), 500, None).summary(),
            "500 rows (more available) · 42 ms"
        );
        assert_eq!(
            info(ready(false), 1200, None).summary(),
            "1200 rows · 42 ms"
        );
        assert_eq!(info(ready(false), 0, Some(3)).summary(), "Done · 42 ms");
        assert_eq!(
            info(QueryStatus::Running, 700, None).summary(),
            "700 rows · running…"
        );
        let failed = QueryStatus::Failed(DriverError::new(ErrorKind::Syntax, "x"));
        assert_eq!(info(failed, 0, None).summary(), "Failed · 42 ms");
        assert!(info(ready(true), 1, None).more_available());
        assert!(!info(QueryStatus::Running, 1, None).more_available());
    }

    #[test]
    fn summary_says_when_a_result_was_cut_short() {
        let cut = ResultInfo {
            cut_short: true,
            ..info(QueryStatus::Ready { more: false }, 500, None)
        };
        assert_eq!(
            cut.summary(),
            "500 rows · 42 ms · stopped early: another query ran on this connection or it \
             closed — re-run to continue"
        );
        assert!(!cut.more_available());
    }
}
