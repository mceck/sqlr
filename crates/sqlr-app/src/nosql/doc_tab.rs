//! A document tab (MongoDB, Elasticsearch): the tab's editor in the connection's language
//! (no SQL completion), and below it the run's results as Table, Tree or JSON (the choice
//! is the tab's), a Messages pane, and a status bar with counts and Fetch more / Fetch all.
//! Edits open the JSON view; saving, inserting and deleting go through the app, so the
//! connection profile can confirm them, and the result is read again once they are done.

use std::collections::HashMap;

use egui::{Margin, RichText};
use sqlr_core::{Cell, ColumnKind, ColumnMeta, DisplayHint, DocTarget, DocValue, QueryLanguage};
use sqlr_engine::{DocResult, Engine, JobId, JobOutcome, QueryId, QueryStatus};
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{
    DocAction, DocTableState, DocTreeState, EditorState, GridInput, GridState, JsonViewState,
    data_grid, doc_table, doc_tree, json_view, value_panel,
};

use super::DocCommand;
use crate::messages::messages_view;
use crate::tabs::TabCommand;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DocView {
    #[default]
    Table,
    Tree,
    Json,
}

impl DocView {
    const ALL: [(DocView, &'static str); 3] = [
        (DocView::Table, "Table"),
        (DocView::Tree, "Tree"),
        (DocView::Json, "JSON"),
    ];
}

/// The widgets' state for one result.
#[derive(Default)]
struct Views {
    table: DocTableState,
    tree: DocTreeState,
    json: JsonViewState,
    /// The grid of a tabular result.
    grid: GridState,
    /// The selected document.
    selected: usize,
}

/// Document ids to delete, by target.
type DeleteGroups = Vec<(DocTarget, Vec<DocValue>)>;

/// An edit job of the tab.
pub(crate) struct DocJob {
    pub id: JobId,
    /// What it does, for the status line.
    pub what: String,
    /// The user action it belongs to: the result is read again once all its jobs worked.
    pub action: u64,
    /// The result whose JSON editor it saves (closed once it worked).
    pub result: Option<QueryId>,
}

pub(crate) struct DocTab {
    pub conn: String,
    /// The connection's language: Elasticsearch documents are identified by `_index` too.
    pub language: QueryLanguage,
    /// The last run's results, one per statement.
    results: Vec<QueryId>,
    /// The statements of the last run, to read the result again after an edit.
    statements: Vec<String>,
    /// The result shown (index into `results`); `None` shows the Messages pane.
    shown: Option<usize>,
    pub view: DocView,
    views: HashMap<QueryId, Views>,
    pub page_size: usize,
    /// Documents waiting for the delete confirmation, grouped by target, and how many.
    delete: Option<(DeleteGroups, usize)>,
    /// Edits running.
    pub jobs: Vec<DocJob>,
    /// The last user action given to the app (`pending_action`), and the actions with a
    /// failed job.
    next_action: u64,
    pub pending_action: Option<(u64, Option<QueryId>)>,
    failed_actions: Vec<u64>,
    /// The outcome of the last edit.
    status: Option<(bool, String)>,
}

/// The editor of a document tab: the connection's language, no SQL completion.
pub(crate) fn doc_editor(editor: &mut EditorState, language: QueryLanguage) {
    editor.set_language(language);
    editor.set_completion(false);
}

impl DocTab {
    pub(crate) fn new(conn: String, language: QueryLanguage, page_size: usize) -> Self {
        Self {
            conn,
            language,
            results: Vec::new(),
            statements: Vec::new(),
            shown: Some(0),
            view: DocView::default(),
            views: HashMap::new(),
            page_size,
            delete: None,
            jobs: Vec::new(),
            next_action: 0,
            pending_action: None,
            failed_actions: Vec::new(),
            status: None,
        }
    }

    /// Starts a user action (a save, an insert, a delete) whose jobs the app adds.
    fn begin_action(&mut self, result: Option<QueryId>) {
        self.next_action += 1;
        self.pending_action = Some((self.next_action, result));
    }

    /// Forgets the last run's results.
    pub(crate) fn close(&mut self, engine: &Engine) {
        for q in self.results.drain(..) {
            engine.close_docs(q);
        }
        self.views.clear();
        self.delete = None;
    }

    /// Adopts a new run; a re-read after an edit (`keep_status`) keeps the edit's outcome.
    pub(crate) fn set_run(
        &mut self,
        engine: &Engine,
        statements: Vec<String>,
        results: Vec<QueryId>,
        keep_status: bool,
    ) {
        self.close(engine);
        self.statements = statements;
        self.results = results;
        self.shown = Some(0);
        if !keep_status {
            self.status = None;
        }
    }

    /// The statements to read the result again after an edit (the app runs them only when
    /// they all read).
    pub(crate) fn rerun(&self) -> Vec<String> {
        self.statements.clone()
    }
}

/// Where a document of `r` is and its `_id`: the result's target, and on Elasticsearch
/// (`es`) its hit's own `_index`; why it cannot be edited otherwise.
fn identity(r: &DocResult, doc: &DocValue, es: bool) -> Result<(DocTarget, DocValue), String> {
    let mut target = r.target.clone().ok_or_else(|| {
        r.read_only
            .clone()
            .unwrap_or_else(|| "the result is read-only".into())
    })?;
    let id = doc
        .get("_id")
        .cloned()
        .ok_or("the document has no `_id`: it is read-only")?;
    if es && let Some(DocValue::String(index)) = doc.get("_index") {
        target.collection = index.clone();
        target.database = None;
    }
    Ok((target, id))
}

/// What a frame of the result asks for, applied once no result guard is held.
enum After {
    FetchMore(QueryId),
    FetchAll(QueryId),
    Cancel(QueryId),
    RowsMore(QueryId),
    Command(DocCommand),
}

/// Draws document tab `tab`: its editor above, the results below.
pub(crate) fn doc_ui(
    ui: &mut egui::Ui,
    tab: u64,
    editor: &mut EditorState,
    d: &mut DocTab,
    engine: &Engine,
    commands: &mut Vec<TabCommand>,
) {
    let p = theme::palette(ui.ctx());
    // The results get the larger share by default, as in a SQL tab.
    let editor_height = theme::editor_split_height(ui.available_height());
    egui::Panel::top(egui::Id::new(("editor_split", tab)))
        .resizable(true)
        .default_size(editor_height)
        .min_size(60.0)
        .frame(egui::Frame::NONE.fill(p.bg_editor))
        .show(ui, |ui| {
            sqlr_ui::query_editor(ui, editor, &sqlr_ui::NoCatalog);
        });
    doc_results_ui(ui, tab, d, engine, commands);
}

/// Draws the result area of document tab `tab`.
fn doc_results_ui(
    ui: &mut egui::Ui,
    tab: u64,
    d: &mut DocTab,
    engine: &Engine,
    commands: &mut Vec<TabCommand>,
) {
    poll_jobs(d, engine, tab, commands);
    let p = theme::palette(ui.ctx());
    if d.results.is_empty() {
        ui.add_space(Spacing::M);
        theme::hint(
            ui,
            "Run the statement at the caret with ⌘↵, or the script with ⇧⌘↵",
        );
        return;
    }
    // Panes: one per result, then Messages; the view switch on the right.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        let many = d.results.len() > 1;
        for i in 0..d.results.len() {
            let label = if many {
                format!("Result {}", i + 1)
            } else {
                "Result".to_string()
            };
            if ui.selectable_label(d.shown == Some(i), label).clicked() {
                d.shown = Some(i);
            }
        }
        if ui.selectable_label(d.shown.is_none(), "Messages").clicked() {
            d.shown = None;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (view, label) in DocView::ALL.iter().rev() {
                if ui.selectable_label(d.view == *view, *label).clicked() {
                    d.view = *view;
                }
            }
        });
    });
    theme::hairline(ui);
    if let Some((ok, text)) = &d.status {
        ui.colored_label(if *ok { p.text_muted } else { p.error }, text);
    }
    let Some(shown) = d.shown else {
        let messages: Vec<_> = d
            .results
            .iter()
            .filter_map(|q| engine.doc_result(*q))
            .flat_map(|r| r.read().messages.clone())
            .collect();
        messages_view(ui, ("doc_messages", tab), &messages);
        return;
    };
    let Some(&q) = d.results.get(shown.min(d.results.len() - 1)) else {
        return;
    };
    let mut after = Vec::new();
    result_ui(ui, d, engine, q, &mut after);
    for a in after {
        match a {
            After::FetchMore(q) => engine.fetch_more_docs(q),
            After::FetchAll(q) => engine.fetch_all_docs(q),
            After::Cancel(q) => engine.cancel_docs(q),
            After::RowsMore(q) => engine.fetch_more(q),
            After::Command(command) => commands.push(TabCommand::Doc { tab, command }),
        }
    }
    delete_confirm(ui, d, tab, commands);
}

/// Ends the edits that finished: their outcome shows. Once every job of an action worked,
/// its JSON editor closes and the result is read again; after a failure the editor keeps
/// the text and nothing is read again.
fn poll_jobs(d: &mut DocTab, engine: &Engine, tab: u64, commands: &mut Vec<TabCommand>) {
    let mut done = Vec::new();
    d.jobs
        .retain(|job| match engine.job(job.id).and_then(|j| j.outcome) {
            Some(outcome) => {
                done.push((job.id, job.what.clone(), job.action, job.result, outcome));
                false
            }
            None => engine.job(job.id).is_some(),
        });
    for (id, what, action, result, outcome) in done {
        engine.forget_job(id);
        let ok = matches!(outcome, JobOutcome::DocsWritten { .. });
        d.status = Some(match outcome {
            JobOutcome::DocsWritten {
                requested, written, ..
            } if written < requested => (
                true,
                format!("{what}: {written} of {requested} (the others no longer existed)"),
            ),
            JobOutcome::DocsWritten { .. } => (true, format!("{what}: done")),
            JobOutcome::Failed(e) => (false, format!("{what} failed: {}", e.message)),
            _ => (false, format!("{what}: cancelled")),
        });
        if !ok {
            d.failed_actions.push(action);
        }
        // The action's last job: close its editor and read again, unless one failed.
        if d.jobs.iter().any(|j| j.action == action) {
            continue;
        }
        if d.failed_actions.contains(&action) {
            d.failed_actions.retain(|a| *a != action);
            continue;
        }
        if let Some(views) = result.and_then(|q| d.views.get_mut(&q)) {
            views.json.cancel();
        }
        let statements = d.rerun();
        if !statements.is_empty() {
            commands.push(TabCommand::Doc {
                tab,
                command: DocCommand::Rerun {
                    conn: d.conn.clone(),
                    statements,
                },
            });
        }
    }
}

fn running_bar(ui: &mut egui::Ui, q: QueryId, after: &mut Vec<After>) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label("Running…");
        if ui.button("Cancel").clicked() {
            after.push(After::Cancel(q));
        }
    });
}

/// The column the value panel shows a field under.
fn field_meta(name: &str) -> ColumnMeta {
    ColumnMeta::new(name, "field", ColumnKind::Text, DisplayHint::Json)
}

fn result_ui(
    ui: &mut egui::Ui,
    d: &mut DocTab,
    engine: &Engine,
    q: QueryId,
    after: &mut Vec<After>,
) {
    let p = theme::palette(ui.ctx());
    let Some(result) = engine.doc_result(q) else {
        theme::hint(ui, "Result closed");
        return;
    };
    let tabular = result.read().tabular;
    if tabular {
        rows_ui(ui, d, engine, q, after);
        return;
    }
    // The guard lives for this frame's drawing; engine calls are deferred (`after`).
    let r = result.read();
    let views = d.views.entry(q).or_default();
    match &r.status {
        QueryStatus::Failed(e) => {
            ui.add_space(Spacing::M);
            ui.colored_label(p.error, &e.message);
            if let Some(hint) = &e.hint {
                ui.label(format!("Hint: {hint}"));
            }
            return;
        }
        QueryStatus::Running if r.is_empty() => {
            running_bar(ui, q, after);
            return;
        }
        _ => {}
    }
    // The status bar: counts, fetch, the reason the documents are read-only.
    let more = r.status == QueryStatus::Ready { more: true };
    egui::Panel::bottom(egui::Id::new(("doc_status", q.0)))
        .frame(egui::Frame::NONE.inner_margin(Margin::symmetric(Spacing::M as i8, 2)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let n = r.len();
                let noun = if n == 1 { "document" } else { "documents" };
                ui.label(format!("{n} {noun}"));
                if more {
                    ui.label(RichText::new("more available").color(p.text_muted));
                    if ui.button("Fetch more").clicked() {
                        after.push(After::FetchMore(q));
                    }
                    if ui.button("Fetch all").clicked() {
                        after.push(After::FetchAll(q));
                    }
                }
                if r.status == QueryStatus::Running {
                    ui.spinner();
                    if ui.button("Cancel").clicked() {
                        after.push(After::Cancel(q));
                    }
                }
                ui.label(
                    RichText::new(format!("{} ms", r.elapsed.as_millis())).color(p.text_muted),
                );
                if let Some(reason) = &r.read_only {
                    ui.label(RichText::new(format!("Read-only: {reason}")).color(p.text_muted));
                } else if ui.button("Insert document").clicked() {
                    views.json.begin_insert(&DocValue::Document(Vec::new()));
                    d.view = DocView::Json;
                }
            });
        });
    let editable = r.read_only.is_none();
    let es = d.language == QueryLanguage::EsConsole;
    let action = match d.view {
        DocView::Table => {
            egui::Panel::right(egui::Id::new(("doc_value", q.0)))
                .resizable(true)
                .default_size(260.0)
                .show(ui, |ui| {
                    let selected = views.table.grid.selected;
                    let rows = views.table.rows(&r.chunks, r.page_size);
                    let cell = selected.and_then(|(row, col)| {
                        Some((rows.columns().get(col)?, rows.cell(row, col)?))
                    });
                    value_panel(ui, cell);
                });
            doc_table(ui, &mut views.table, &r.chunks, r.page_size, more)
        }
        DocView::Tree => {
            egui::Panel::right(egui::Id::new(("doc_field", q.0)))
                .resizable(true)
                .default_size(260.0)
                .show(ui, |ui| match views.tree.selected_field() {
                    Some((path, value)) => {
                        let text = value.to_relaxed_json_pretty();
                        value_panel(ui, Some((&field_meta(&path), Cell::Text(&text))));
                    }
                    None => value_panel(ui, None),
                });
            doc_tree(ui, &mut views.tree, &r.chunks, editable)
        }
        DocView::Json => {
            let i = views.selected.min(r.len().saturating_sub(1));
            match r.doc(i) {
                Some(doc) => {
                    let can_edit = editable && doc.get("_id").is_some();
                    json_view(ui, &mut views.json, i, &doc, can_edit)
                }
                None if views.json.editing() => json_view(
                    ui,
                    &mut views.json,
                    0,
                    &DocValue::Document(Vec::new()),
                    editable,
                ),
                None => {
                    theme::hint(ui, "No documents");
                    None
                }
            }
        }
    };
    let Some(action) = action else {
        return;
    };
    match action {
        DocAction::Select(i) => views.selected = i,
        DocAction::LoadMore => {
            if more {
                after.push(After::FetchMore(q));
            }
        }
        DocAction::Edit(i) => {
            views.selected = i;
            if let Some(doc) = r.doc(i) {
                match identity(&r, &doc, es) {
                    Ok(_) => {
                        views.json.begin_edit(i, &doc);
                        d.view = DocView::Json;
                    }
                    Err(why) => d.status = Some((false, format!("Document {}: {why}", i + 1))),
                }
            }
        }
        DocAction::CopyJson(i) => {
            if let Some(doc) = r.doc(i) {
                ui.ctx().copy_text(doc.to_relaxed_json_pretty());
            }
        }
        DocAction::Save(i, doc) => match r.doc(i).map(|old| identity(&r, &old, es)) {
            Some(Ok((target, id))) => {
                d.begin_action(Some(q));
                after.push(After::Command(DocCommand::Save {
                    conn: d.conn.clone(),
                    target,
                    id: Some(id),
                    doc,
                }));
            }
            Some(Err(why)) => d.status = Some((false, format!("Not saved: {why}"))),
            None => d.status = Some((false, "Not saved: the document is gone".into())),
        },
        DocAction::Insert(doc) => {
            if let Some(target) = r.target.clone() {
                if d.view == DocView::Json {
                    d.begin_action(Some(q));
                    after.push(After::Command(DocCommand::Save {
                        conn: d.conn.clone(),
                        target,
                        id: None,
                        doc,
                    }));
                } else {
                    // "Duplicate" opens the copy in the JSON view first.
                    views.json.begin_insert(&doc);
                    d.view = DocView::Json;
                }
            }
        }
        DocAction::Delete(docs) => {
            // Identities first: the confirmation counts what will be deleted.
            let mut groups: DeleteGroups = Vec::new();
            let mut skipped = 0;
            for i in docs {
                match r.doc(i).map(|doc| identity(&r, &doc, es)) {
                    Some(Ok((target, id))) => match groups.iter_mut().find(|(t, _)| *t == target) {
                        Some((_, ids)) => ids.push(id),
                        None => groups.push((target, vec![id])),
                    },
                    _ => skipped += 1,
                }
            }
            let n: usize = groups.iter().map(|(_, ids)| ids.len()).sum();
            if skipped > 0 {
                d.status = Some((
                    false,
                    format!("{skipped} documents without `_id` are read-only"),
                ));
            }
            if n > 0 {
                d.delete = Some((groups, n));
            }
        }
    }
}

/// A tabular result (`distinct`, ES|QL, SQL): the ordinary grid.
fn rows_ui(ui: &mut egui::Ui, d: &mut DocTab, engine: &Engine, q: QueryId, after: &mut Vec<After>) {
    let p = theme::palette(ui.ctx());
    let Some(result) = engine.result(q) else {
        theme::hint(ui, "Result closed");
        return;
    };
    let r = result.read();
    if let QueryStatus::Failed(e) = &r.status {
        ui.colored_label(p.error, &e.message);
        return;
    }
    if r.status == QueryStatus::Running && r.rows.row_count() == 0 {
        running_bar(ui, q, after);
        return;
    }
    let more = r.status == QueryStatus::Ready { more: true };
    ui.label(format!(
        "{} rows{}",
        r.rows.row_count(),
        if more { " (more available)" } else { "" }
    ));
    let views = d.views.entry(q).or_default();
    let out = data_grid(
        ui,
        &mut views.grid,
        &GridInput {
            rows: &r.rows,
            more_available: more,
            dialect: None,
            table: None,
        },
    );
    views.grid.take_actions();
    if out.need_more && more {
        after.push(After::RowsMore(q));
    }
}

/// "Delete N documents?": confirmed deletes go to the app, grouped by their target, as one
/// action (one re-read).
fn delete_confirm(ui: &mut egui::Ui, d: &mut DocTab, tab: u64, commands: &mut Vec<TabCommand>) {
    let Some((_, n)) = &d.delete else {
        return;
    };
    let n = *n;
    let mut answer = None;
    let modal = egui::Modal::new(egui::Id::new(("doc_delete", tab))).show(ui.ctx(), |ui| {
        let noun = if n == 1 { "document" } else { "documents" };
        ui.label(format!("Delete {n} {noun}?"));
        ui.add_space(Spacing::L);
        ui.horizontal(|ui| {
            if ui.button("Delete").clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
    });
    if answer.is_none() && modal.should_close() {
        answer = Some(false);
    }
    let Some(yes) = answer else {
        return;
    };
    let Some((groups, _)) = d.delete.take() else {
        return;
    };
    if yes {
        d.begin_action(None);
        commands.push(TabCommand::Doc {
            tab,
            command: DocCommand::Delete {
                conn: d.conn.clone(),
                groups,
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(target: &str) -> DocResult {
        let mut r = DocResult::new(String::new(), 10, QueryStatus::Ready { more: false });
        r.target = Some(DocTarget {
            database: Some("app".into()),
            collection: target.into(),
        });
        r
    }

    #[test]
    fn identity_uses_index_only_on_elasticsearch() {
        let doc = DocValue::Document(vec![
            ("_id".into(), DocValue::Int32(1)),
            ("_index".into(), DocValue::String("other".into())),
        ]);
        let r = result("users");
        let (mongo, _) = identity(&r, &doc, false).unwrap();
        assert_eq!(
            mongo.collection, "users",
            "a Mongo field named _index is data"
        );
        let (es, id) = identity(&r, &doc, true).unwrap();
        assert_eq!((es.collection.as_str(), id), ("other", DocValue::Int32(1)));
        let no_id = DocValue::Document(vec![("a".into(), DocValue::Int32(1))]);
        assert!(
            identity(&r, &no_id, false)
                .unwrap_err()
                .contains("no `_id`")
        );
    }
}
