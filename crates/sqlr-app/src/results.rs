//! Editing a result (§7.7, Decisions "Inline editing scope" and "Apply"): whether it can be
//! edited and why not, the Preview dialog, applying the changeset and re-running the
//! result; plus the grid's other requests (server sort and filter, large copies, the value
//! panel's editor).

use sqlr_core::{Changeset, SqlSyntax, TableRef};
use sqlr_engine::{Engine, JobOutcome, LoadState, QueryId};
use sqlr_ui::ToastKind;
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{
    ChangesAction, CopyFormat, EditBuffer, EditorState, GridAction, GridState, MAX_COPY_CELLS,
    NoCatalog, ValueEdit, ValueEditAction, query_editor, value_editor, value_panel,
};

use crate::app::{SqlrApp, tab_mut};
use crate::tabs::{QueryTab, ServerView, TabRun};
use crate::transfer_dialogs::{JobWhat, TrackedJob};

/// Whether a result can be edited, as far as it is known this frame.
enum Editability {
    ReadOnly(String),
    /// Waiting for the table's description.
    Pending(TableRef),
    Editable(TableRef, sqlr_core::TableInfo),
}

/// Works out (each frame, cheaply) whether the result `q` can be edited, asks the engine for
/// its table's description when it has none yet, and gives the grid an edit buffer once it
/// can. Call it without holding the result's guard.
pub(crate) fn refresh_editability(
    tab: &mut QueryTab,
    engine: &Engine,
    q: QueryId,
    driver_edits: bool,
) {
    let Some(conn) = tab.conn.clone() else {
        return;
    };
    let columns = {
        let Some(result) = engine.result(q) else {
            return;
        };
        let r = result.read();
        if r.rows.columns().is_empty() || matches!(r.status, sqlr_engine::QueryStatus::Failed(_)) {
            return;
        }
        r.rows.columns().to_vec()
    };
    let key = tab.session(engine, &conn);
    let mut origins = columns.iter().filter_map(|c| c.origin.as_ref());
    let first = origins.next();
    let state = if !driver_edits {
        Editability::ReadOnly("driver does not support editing".into())
    } else if engine.transaction(&key).is_some_and(|t| t.read_only) {
        Editability::ReadOnly("session is read-only".into())
    } else {
        match first {
            None => Editability::ReadOnly("computed columns".into()),
            Some(o) if origins.any(|other| other.table != o.table) => {
                Editability::ReadOnly("columns from several tables".into())
            }
            Some(o) => {
                let mut table = o.table.clone();
                if table.database.is_none() {
                    table.database = tab.database.clone();
                }
                match engine.table_info(&conn, &table) {
                    None => {
                        engine.describe(&key, table.clone());
                        Editability::Pending(table)
                    }
                    Some(LoadState::Loading) => Editability::Pending(table),
                    Some(LoadState::Failed(e)) => Editability::ReadOnly(format!(
                        "the description of {} failed: {}",
                        table.name, e.message
                    )),
                    Some(LoadState::Ready(info)) => Editability::Editable(table, info),
                }
            }
        }
    };
    let edit = tab.edits.entry(q).or_default();
    let grid = tab.grids.entry(q).or_default();
    match state {
        Editability::ReadOnly(reason) => {
            edit.read_only = Some(reason);
            edit.table = first.map(|o| o.table.clone());
            grid.edits = None;
        }
        Editability::Pending(table) => {
            edit.table = Some(table);
            edit.read_only = None;
        }
        Editability::Editable(table, info) => {
            edit.table = Some(table);
            match EditBuffer::for_table(&columns, &info) {
                Ok(buffer) => {
                    edit.read_only = None;
                    if grid.edits.is_none() {
                        grid.edits = Some(buffer);
                    }
                }
                Err(reason) => {
                    edit.read_only = Some(reason);
                    grid.edits = None;
                }
            }
        }
    }
}

/// The Preview dialog: the changeset's statements in a read-only editor, and Apply.
pub(crate) struct ChangesDialog {
    tab: u64,
    q: QueryId,
    text: String,
    editor: EditorState,
    syntax: std::sync::Arc<SqlSyntax>,
    /// Opened by Apply because the profile asks before writing.
    confirm: bool,
}

/// A copy too large to start without asking.
pub(crate) struct PendingCopy {
    q: QueryId,
    rows: Vec<usize>,
    cols: Vec<usize>,
    format: sqlr_core::TextFormat,
}

impl PendingCopy {
    fn cells(&self) -> usize {
        self.rows.len() * self.cols.len()
    }
}

impl SqlrApp {
    /// The statements of the open Preview dialog.
    #[doc(hidden)]
    pub fn changes_preview(&self) -> Option<String> {
        self.changes_dialog.as_ref().map(|d| d.text.clone())
    }

    /// Answers every native file dialog with `path` instead of opening it (tests).
    #[doc(hidden)]
    pub fn set_file_dialog_answer(&mut self, path: Option<std::path::PathBuf>) {
        self.file_answer = path;
    }

    /// The changeset of the result's pending edits.
    fn changeset(&mut self, tab: u64, q: QueryId) -> Result<Changeset, String> {
        let t = tab_mut(&mut self.dock, tab).ok_or("The tab is closed")?;
        let table = t
            .edits
            .get(&q)
            .and_then(|e| e.table.clone())
            .ok_or("The result's table is unknown")?;
        let edits = t
            .grids
            .get(&q)
            .and_then(|g| g.edits.as_ref())
            .ok_or("The result cannot be edited")?;
        let result = self.engine.result(q).ok_or("The result is closed")?;
        let r = result.read();
        edits.changeset(&r.rows, table)
    }

    pub(crate) fn changes_action(&mut self, tab: u64, q: QueryId, action: ChangesAction) {
        match action {
            ChangesAction::Preview => self.open_changes(tab, q, false),
            ChangesAction::Apply => {
                let confirm = tab_mut(&mut self.dock, tab)
                    .and_then(|t| t.conn.clone())
                    .and_then(|c| self.connection(&c).cloned())
                    .is_some_and(|c| self.profile_for(&c).confirm_writes);
                if confirm {
                    self.open_changes(tab, q, true);
                } else {
                    self.start_apply(tab, q);
                }
            }
            ChangesAction::Discard => {
                if let Some(t) = tab_mut(&mut self.dock, tab) {
                    if let Some(edits) = t.grids.get_mut(&q).and_then(|g| g.edits.as_mut()) {
                        edits.clear();
                    }
                    t.value_edit = None;
                }
            }
        }
    }

    fn open_changes(&mut self, tab: u64, q: QueryId, confirm: bool) {
        let changeset = match self.changeset(tab, q) {
            Ok(c) => c,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        let Some(conn) = tab_mut(&mut self.dock, tab).and_then(|t| t.conn.clone()) else {
            return;
        };
        let Some(dialect) = self.dialects.get(&conn).cloned() else {
            self.notify("No SQL dialect for this connection");
            return;
        };
        let text = match changeset.preview(&*dialect) {
            Ok(text) => text,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        let syntax = dialect.syntax();
        self.changes_dialog = Some(ChangesDialog {
            tab,
            q,
            editor: EditorState::new(&text, syntax.clone()),
            text,
            syntax,
            confirm,
        });
    }

    fn start_apply(&mut self, tab: u64, q: QueryId) {
        let changeset = match self.changeset(tab, q) {
            Ok(c) => c,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        let engine = &self.engine;
        let Some(t) = tab_mut(&mut self.dock, tab) else {
            return;
        };
        let Some(conn) = t.conn.clone() else {
            return;
        };
        let key = t.session(engine, &conn);
        let job = engine.apply_changeset(&key, changeset);
        t.edits.entry(q).or_default().applying = Some(job);
    }

    pub(crate) fn changes_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.changes_dialog else {
            return;
        };
        let (tab, q) = (dialog.tab, dialog.q);
        let mut open = true;
        let mut apply = false;
        let mut close = false;
        theme::tool_dialog(
            ctx,
            &mut open,
            "Pending changes",
            theme::icons::EDIT,
            "Review the SQL before applying your edits",
            720.0,
            |ui| {
                if dialog.confirm {
                    ui.colored_label(
                        theme::palette(ui.ctx()).warning,
                        "This connection's profile asks before writing: review the changes.",
                    );
                }
                ui.allocate_ui(egui::vec2(ui.available_width(), 220.0), |ui| {
                    query_editor(ui, &mut dialog.editor, &NoCatalog);
                });
                // Read-only: typing is undone.
                if dialog.editor.text() != dialog.text {
                    dialog.editor = EditorState::new(&dialog.text, dialog.syntax.clone());
                }
                ui.add_space(Spacing::S);
                ui.horizontal(|ui| {
                    if ui
                        .add(theme::primary_button(ui.ctx(), "Apply changes"))
                        .clicked()
                    {
                        apply = true;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            },
        );
        if apply {
            self.changes_dialog = None;
            self.start_apply(tab, q);
        } else if close || !open {
            self.changes_dialog = None;
        }
    }

    /// Ends the changesets that finished applying: a success clears the edits and re-runs
    /// the result; a failure keeps them and says why.
    pub(crate) fn poll_applies(&mut self) {
        let engine = &self.engine;
        let mut done = Vec::new();
        for (_, tab) in self.dock.iter_all_tabs_mut() {
            for (q, edit) in &mut tab.edits {
                let Some(job) = edit.applying else {
                    continue;
                };
                if let Some(outcome) = engine.job(job).and_then(|j| j.outcome) {
                    edit.applying = None;
                    engine.forget_job(job);
                    done.push((tab.key, *q, outcome));
                }
            }
        }
        for (tab, q, outcome) in done {
            match outcome {
                JobOutcome::Applied(report) => match report.failed {
                    None => {
                        let noun = if report.applied == 1 {
                            "change"
                        } else {
                            "changes"
                        };
                        self.notify_as(
                            ToastKind::Success,
                            format!("Applied {} {noun}", report.applied),
                        );
                        self.rerun_result(tab, q);
                    }
                    Some((row, e)) => {
                        let message = if e.message.contains(&row) {
                            e.message
                        } else {
                            format!("{row}: {}", e.message)
                        };
                        self.notify(format!("Nothing was applied. {message}"));
                    }
                },
                JobOutcome::Failed(e) => {
                    self.notify(format!("Nothing was applied. {}", e.message));
                }
                _ => {}
            }
        }
    }

    /// Runs the result's statement again as the tab's run, keeping its server sort and
    /// filter and the grid's local ones.
    fn rerun_result(&mut self, tab: u64, q: QueryId) {
        let engine = &self.engine;
        let Some(t) = tab_mut(&mut self.dock, tab) else {
            return;
        };
        let Some(conn) = t.conn.clone() else {
            return;
        };
        let Some(sql) = engine.result(q).map(|r| r.read().sql.clone()) else {
            return;
        };
        let origin = match &t.run {
            Some(TabRun::Single { q: current, origin }) if *current == q => *origin,
            _ => None,
        };
        let seed = t
            .grids
            .get(&q)
            .map(|g| (g.sort.clone(), g.where_filter.clone()));
        let server = t.server.clone();
        let pane = t.pane;
        let new = t.run_sql(engine, &conn, sql, origin);
        t.server = server;
        t.pane = pane;
        if let Some((sort, filter)) = seed {
            let grid = t.grids.entry(new).or_default();
            grid.sort = sort;
            grid.where_filter = filter;
        }
    }

    pub(crate) fn grid_action(&mut self, tab: u64, q: QueryId, action: GridAction) {
        match action {
            GridAction::SortOnServer(order) => self.server_rerun(tab, q, Some(order), None),
            GridAction::FilterOnServer(filter) => self.server_rerun(tab, q, None, Some(filter)),
            GridAction::Copy(format) => self.copy_request(tab, q, format),
            GridAction::EditCell { row, col } => {
                let engine = &self.engine;
                let Some(t) = tab_mut(&mut self.dock, tab) else {
                    return;
                };
                let Some(result) = engine.result(q) else {
                    return;
                };
                let r = result.read();
                let (Some(meta), Some(grid)) = (r.rows.columns().get(col), t.grids.get(&q)) else {
                    return;
                };
                let value = grid
                    .edits
                    .as_ref()
                    .and_then(|e| e.value(&r.rows, row, col))
                    .unwrap_or(sqlr_core::Value::Null);
                t.value_edit = Some((q, ValueEdit::new(row, col, meta, &value)));
                drop(r);
                self.show_value_panel = true;
            }
            GridAction::OpenValue { .. } => self.show_value_panel = true,
            GridAction::PasteFailed(message) => self.notify_as(ToastKind::Error, message),
            GridAction::Sort(_)
            | GridAction::SetCell { .. }
            | GridAction::AddRow
            | GridAction::DuplicateRow(_)
            | GridAction::DeleteRows(_)
            | GridAction::RevertRows(_) => {}
        }
    }

    /// Re-runs the tab's statement sorted and filtered by the server (§7.5): `Dialect::
    /// wrap_where` and `wrap_order` over the statement as the user ran it.
    fn server_rerun(
        &mut self,
        tab: u64,
        q: QueryId,
        order: Option<Vec<(usize, bool)>>,
        filter: Option<String>,
    ) {
        let engine = &self.engine;
        let Some(t) = tab_mut(&mut self.dock, tab) else {
            return;
        };
        let Some(conn) = t.conn.clone() else {
            return;
        };
        // A re-run replaces the result, and its pending edits with it.
        let pending = t
            .grids
            .get(&q)
            .and_then(|g| g.edits.as_ref())
            .is_some_and(|e| e.pending() > 0);
        if pending {
            self.notify_as(
                ToastKind::Info,
                "Apply or discard the pending changes first",
            );
            return;
        }
        let Some(dialect) = self.dialects.get(&conn).cloned() else {
            self.notify("No SQL dialect for this connection");
            return;
        };
        let Some((sql, names)) = engine.result(q).map(|r| {
            let r = r.read();
            let names: Vec<String> = r.rows.columns().iter().map(|c| c.name.clone()).collect();
            (r.sql.clone(), names)
        }) else {
            return;
        };
        let mut view = t.server.clone().unwrap_or(ServerView {
            base: sql,
            ..ServerView::default()
        });
        if let Some(order) = order {
            view.order = order
                .iter()
                .filter_map(|(c, desc)| Some((*c, names.get(*c)?.clone(), *desc)))
                .collect();
        }
        if let Some(filter) = filter {
            view.filter = filter;
        }
        let mut sql = view.base.clone();
        if !view.filter.is_empty() {
            sql = dialect.wrap_where(&sql, &view.filter);
        }
        if !view.order.is_empty() {
            let order: Vec<(String, bool)> =
                view.order.iter().map(|(_, n, d)| (n.clone(), *d)).collect();
            sql = dialect.wrap_order(&sql, &order);
        }
        let new = t.run_sql(engine, &conn, sql, None);
        let grid: &mut GridState = t.grids.entry(new).or_default();
        grid.sort = view.order.iter().map(|(c, _, d)| (*c, *d)).collect();
        grid.where_filter = view.filter.clone();
        t.server = Some(view);
    }

    fn copy_request(&mut self, tab: u64, q: QueryId, format: CopyFormat) {
        let Some(t) = tab_mut(&mut self.dock, tab) else {
            return;
        };
        let Some(grid) = t.grids.get(&q) else {
            return;
        };
        let table = t
            .edits
            .get(&q)
            .and_then(|e| e.table.as_ref())
            .map(|t| t.name.clone());
        let pending = PendingCopy {
            q,
            rows: grid.selected_rows(),
            cols: grid.selected_cols().collect(),
            format: format.text_format(grid.whole_columns(), table.as_deref()),
        };
        if pending.cells() > MAX_COPY_CELLS {
            self.copy_pending = Some(pending);
        } else {
            self.start_copy(pending);
        }
    }

    fn start_copy(&mut self, pending: PendingCopy) {
        let cells = pending.cells();
        let id = self
            .engine
            .copy_text(pending.q, pending.rows, pending.cols, pending.format);
        self.jobs.push(TrackedJob {
            id,
            what: JobWhat::Copy { cells },
        });
    }

    /// Asks before copying more than [`MAX_COPY_CELLS`] cells.
    pub(crate) fn copy_confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.copy_pending else {
            return;
        };
        let cells = pending.cells();
        let mut choice = None;
        egui::Window::new("Copy a large selection")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("Copy {cells} cells to the clipboard?"));
                ui.horizontal(|ui| {
                    if ui.button("Copy").clicked() {
                        choice = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(false);
                    }
                });
            });
        match choice {
            Some(true) => {
                if let Some(pending) = self.copy_pending.take() {
                    self.start_copy(pending);
                }
            }
            Some(false) => self.copy_pending = None,
            None => {}
        }
    }

    /// The value panel: the cell being edited, else the selected cell (its pending value
    /// when it has one).
    pub(crate) fn value_ui(&mut self, ui: &mut egui::Ui) {
        let Some(key) = self.active_key() else {
            value_panel(ui, None);
            return;
        };
        let engine = &self.engine;
        let Some(tab) = tab_mut(&mut self.dock, key) else {
            value_panel(ui, None);
            return;
        };
        // The sessions tab shows the selected session's query.
        if let Some(admin) = &tab.admin {
            let meta = crate::admin::query_meta();
            let query = admin
                .selected_query()
                .map(|q| (&meta, sqlr_core::Cell::Text(q)));
            value_panel(ui, query);
            return;
        }
        let Some(q) = tab.shown_query(engine) else {
            value_panel(ui, None);
            return;
        };
        let Some(result) = engine.result(q) else {
            value_panel(ui, None);
            return;
        };
        if let Some((edit_q, edit)) = &mut tab.value_edit
            && *edit_q == q
        {
            let r = result.read();
            let Some(meta) = r.rows.columns().get(edit.col).cloned() else {
                drop(r);
                tab.value_edit = None;
                return;
            };
            let not_null = tab
                .grids
                .get(&q)
                .and_then(|g| g.edits.as_ref())
                .is_some_and(|e| e.not_null(edit.col));
            let action = value_editor(ui, &meta, edit, not_null);
            let (row, col) = (edit.row, edit.col);
            let value = match action {
                None => return,
                Some(ValueEditAction::Cancel) => {
                    tab.value_edit = None;
                    return;
                }
                Some(ValueEditAction::Save(v)) => v,
                Some(ValueEditAction::SetNull) => sqlr_core::Value::Null,
            };
            let outcome = match tab.grids.get_mut(&q) {
                Some(grid) => grid.set_cell(&r.rows, row, col, value),
                None => Err("The result is closed".into()),
            };
            drop(r);
            match outcome {
                Ok(()) => tab.value_edit = None,
                Err(e) => {
                    if let Some((_, edit)) = &mut tab.value_edit {
                        edit.error = Some(e);
                    }
                }
            }
            return;
        }
        let Some(grid) = tab.grids.get(&q) else {
            value_panel(ui, None);
            return;
        };
        let Some((row, col)) = grid.selected else {
            value_panel(ui, None);
            return;
        };
        let r = result.read();
        let Some(meta) = r.rows.columns().get(col) else {
            value_panel(ui, None);
            return;
        };
        let edited = grid.edits.as_ref().and_then(|e| e.value(&r.rows, row, col));
        match edited {
            Some(value) => value_panel(ui, Some((meta, value.as_cell()))),
            None => value_panel(ui, r.rows.cell(row, col).map(|cell| (meta, cell))),
        }
    }
}
