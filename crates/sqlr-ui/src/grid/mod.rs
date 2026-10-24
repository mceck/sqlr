//! Custom-painted, virtualized result grid (rows and columns): row numbers, rectangular
//! selection with keyboard navigation, local sort (server sorting offered as an action),
//! copy in several formats, and inline editing over an [`EditBuffer`].
//!
//! The widget calls no engine: it reads the rows it is given, keeps its own state, and
//! queues [`GridAction`]s for the caller ([`GridState::take_actions`]).

mod clipboard;
mod doc_table;
mod edit;
mod header;
mod operations;
mod selection;
mod sort;
mod sort_worker;

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob};
use egui::{Color32, Event, Key, Modifiers, Rect, RichText, Sense, Stroke, TextEdit, pos2, vec2};
use sqlr_core::{
    Cell, ColumnKind, ColumnMeta, DataFormats, Dialect, DisplayHint, RowBuffer, Value,
};

pub use clipboard::set_grid_clipboard;
pub use doc_table::{DocTableState, doc_table};
pub use edit::{EditBuffer, INSERTED_ROW_BASE, RowState, is_inserted_row, parse_hex, parse_input};
use header::{Geometry, HEADER_HEIGHT, Hit};
use selection::subtract;
pub use selection::{CopyFormat, Selection};
use sort::{RowOrder, ViewKey};

use crate::theme::{self, Spacing, icons};

/// Rows before the end of the buffer that trigger `need_more`.
pub const NEED_MORE_MARGIN: usize = 50;
/// Display truncation per cell, in chars.
pub const MAX_CELL_CHARS: usize = 200;
/// Bytes shown in a grid cell.
pub const MAX_CELL_BYTES: usize = 32;
/// Copying more cells than this asks first (the app confirms).
pub const MAX_COPY_CELLS: usize = 100_000;
/// Clipboard text larger than this (estimated) is built off the UI thread by the app.
pub const MAX_COPY_BYTES: usize = 1 << 20;

const ROW_HEIGHT: f32 = Spacing::ROW_GRID;
pub(crate) const CELL_PADDING: f32 = Spacing::M;
const MIN_COL_WIDTH: f32 = 60.0;
const MAX_COL_WIDTH: f32 = 400.0;
const MIN_GUTTER: f32 = 36.0;
const WIDTH_SAMPLE_ROWS: usize = 100;
const OVERSCAN_ROWS: usize = 2;
const NULL_TEXT: &str = "NULL";
/// Shown in the cells of an inserted row left to their default.
const DEFAULT_TEXT: &str = "DEFAULT";
/// Values longer than this (or with line breaks) are edited in the value panel.
const INLINE_EDIT_CHARS: usize = 200;

/// `color` at 25 % alpha: the tint of edited cells (`warning`), inserted rows (`success`)
/// and deleted rows (`error`), over the row background.
pub fn change_tint(color: Color32) -> Color32 {
    let [r, g, b, _] = color.to_array();
    Color32::from_rgba_unmultiplied(r, g, b, 64)
}

#[derive(Default)]
pub struct GridState {
    /// The cell under the selection's cursor, as `(source row, column)`: a row of the
    /// buffer, or an inserted row's number (see [`INSERTED_ROW_BASE`]). `None` without a
    /// selection.
    pub selected: Option<(usize, usize)>,
    /// The primary selected rectangle, in view coordinates; meaningful while `selected`
    /// is set. Its cursor is the active cell.
    pub selection: Selection,
    /// Additional rectangles selected with ⌘, in view coordinates.
    extra: Vec<Selection>,
    history: operations::EditHistory,
    paste_new_rows: bool,
    paste_dialog: Option<clipboard::PasteRowsDialog>,
    /// `(column, descending)`; applied locally when the result is complete.
    pub sort: Vec<(usize, bool)>,
    /// The server-side condition typed into the WHERE field.
    pub where_filter: String,
    /// Pending edits; `Some` when the result can be edited.
    pub edits: Option<EditBuffer>,
    /// Rows inside the viewport during the last frame (without overscan).
    pub last_visible_rows: Range<usize>,
    /// Scrolls this row to the top on the next frame, then is cleared.
    pub scroll_to_row: Option<usize>,
    /// A source row to reveal (select and scroll to) once the view is rebuilt.
    reveal: Option<usize>,
    layout: Option<ColumnLayout>,
    view: Option<(ViewKey, RowOrder)>,
    sort_worker: Option<sort_worker::SortWorker>,
    sorting: Option<ViewKey>,
    sort_repaint: Option<egui::Context>,
    editor: Option<CellEditor>,
    /// A sort waiting for "Sort on server".
    server_sort: Option<Vec<(usize, bool)>>,
    actions: Vec<GridAction>,
    /// Give the keyboard back to the grid on the next frame.
    refocus: bool,
}

/// Column widths for one column set, measured once and reused every frame.
struct ColumnLayout {
    key: u64,
    /// `x[i]` is the left edge of column `i`; the last entry is the total width.
    x: Vec<f32>,
}

/// The inline single-line editor over one cell.
struct CellEditor {
    /// Source row.
    row: usize,
    col: usize,
    text: String,
    error: Option<String>,
    /// Focus it on the next frame.
    focus: bool,
    /// Select its whole text once it has the focus.
    select_all: bool,
}

/// What the grid shows this frame.
pub struct GridInput<'a> {
    pub rows: &'a RowBuffer,
    /// The query may return more rows than are loaded.
    pub more_available: bool,
    /// Writes literals ("Filter by this value", SQL `INSERT` copies); ANSI without one.
    pub dialect: Option<&'a dyn Dialect>,
    /// The table SQL `INSERT` copies write into (`"result"` without one).
    pub table: Option<&'a str>,
}

/// What the grid asks of its caller. Rows are source rows: loaded rows by index, inserted
/// rows by their number (from [`INSERTED_ROW_BASE`]). Edits are already recorded in [`GridState::edits`] when they are reported.
#[derive(Clone, Debug, PartialEq)]
pub enum GridAction {
    /// The rows were sorted locally.
    Sort(Vec<(usize, bool)>),
    /// Re-run the query sorted by the server (the result is partial).
    SortOnServer(Vec<(usize, bool)>),
    /// The selection is too large to copy on the UI thread: confirm, then copy through the
    /// engine (see [`GridState::selected_rows`]).
    Copy(CopyFormat),
    /// Re-run the current read query as a cancellable clipboard job.
    CopyAll,
    /// An operation (including undo/redo) replaced the pending edits.
    EditsChanged,
    /// Edit the cell in the value panel (multi-line, long, JSON or binary values).
    EditCell {
        row: usize,
        col: usize,
    },
    /// A paste was refused; the caller shows the reason.
    PasteFailed(String),
    SetCell {
        row: usize,
        col: usize,
        value: Value,
    },
    AddRow,
    DuplicateRow(usize),
    DeleteRows(Vec<usize>),
    RevertRows(Vec<usize>),
    /// Re-run the query with this `WHERE` condition (empty: without one).
    FilterOnServer(String),
    /// Show the cell in the value panel.
    OpenValue {
        row: usize,
        col: usize,
    },
}

pub struct GridOutput {
    /// The caller should fetch the next page.
    pub need_more: bool,
    pub selection_changed: bool,
}

impl GridState {
    /// The actions queued since the last call.
    pub fn take_actions(&mut self) -> Vec<GridAction> {
        std::mem::take(&mut self.actions)
    }

    /// The source rows shown, in order (as of the last frame).
    pub fn view_rows(&self) -> Vec<usize> {
        self.view
            .as_ref()
            .map(|(_, v)| v.iter().collect())
            .unwrap_or_default()
    }

    /// The source rows of the selection, in view order; empty without a selection.
    pub fn selected_rows(&self) -> Vec<usize> {
        let Some((_, view)) = &self.view else {
            return Vec::new();
        };
        self.selected_view_rows()
            .into_iter()
            .filter_map(|r| view.get(r))
            .collect()
    }

    /// The view rows with at least one selected cell, in order.
    fn selected_view_rows(&self) -> Vec<usize> {
        if self.selected.is_none() {
            return Vec::new();
        }
        let mut rows = BTreeSet::new();
        for selection in self.all_selections() {
            rows.extend(selection.rows());
        }
        rows.into_iter().collect()
    }

    /// The columns with at least one selected cell, in order.
    pub fn selected_cols(&self) -> Vec<usize> {
        if self.selected.is_none() {
            return Vec::new();
        }
        let mut cols = BTreeSet::new();
        for selection in self.all_selections() {
            cols.extend(selection.cols());
        }
        cols.into_iter().collect()
    }

    /// The selection spans every shown row (whole columns, or everything).
    pub fn whole_columns(&self) -> bool {
        let shown = self.view.as_ref().map_or(0, |(_, v)| v.len());
        self.selected.is_some() && shown > 0 && self.selection.rows() == (0..=shown - 1)
    }

    /// The number of selected cells; the rectangles never overlap.
    fn selected_cells(&self) -> usize {
        if self.selected.is_none() {
            return 0;
        }
        self.all_selections().map(|s| s.cell_count()).sum()
    }

    /// Whether a view cell is part of any selected rectangle.
    pub fn is_selected(&self, row: usize, col: usize) -> bool {
        self.selected.is_some()
            && self
                .all_selections()
                .any(|selection| selection.contains(row, col))
    }

    /// The primary rectangle and the ones added with ⌘.
    fn all_selections(&self) -> impl Iterator<Item = Selection> + '_ {
        std::iter::once(self.selection).chain(self.extra.iter().copied())
    }

    /// Replaces every rectangle, the first becoming the primary one.
    fn set_selections(&mut self, mut selections: Vec<Selection>) {
        if selections.is_empty() {
            self.selected = None;
            self.selection = Selection::default();
            self.extra.clear();
            return;
        }
        self.selection = selections.remove(0);
        self.extra = selections;
        self.sync_selected();
    }

    /// Adds a rectangle, subtracting its area from the rectangles already selected.
    fn add_selection(&mut self, selection: Selection) {
        let mut kept = Vec::new();
        if self.selected.is_some() {
            for existing in self.all_selections() {
                kept.extend(subtract(&existing, &selection));
            }
        }
        self.selection = selection;
        self.extra = kept;
        self.sync_selected();
    }

    /// Removes a rectangle from everything selected.
    fn remove_selection(&mut self, selection: Selection) {
        if self.selected.is_none() {
            return;
        }
        let mut kept = Vec::new();
        for existing in self.all_selections() {
            kept.extend(subtract(&existing, &selection));
        }
        self.set_selections(kept);
    }

    /// ⌘+click on a cell: adds it, or removes it when it is already selected.
    fn toggle_cell(&mut self, row: usize, col: usize) -> bool {
        if self.is_selected(row, col) {
            self.remove_selection(Selection::cell(row, col));
        } else {
            self.add_selection(Selection::cell(row, col));
        }
        true
    }

    /// ⌘+click on a row number.
    fn toggle_row(&mut self, row: usize, last_col: usize) -> bool {
        let whole = Selection {
            anchor: (row, 0),
            cursor: (row, last_col),
        };
        let full = (0..=last_col).all(|col| self.is_selected(row, col));
        if full {
            self.remove_selection(whole);
        } else {
            self.add_selection(whole);
        }
        true
    }

    /// ⌘+click on a column header.
    fn toggle_column(&mut self, col: usize, last_row: usize) -> bool {
        let whole = Selection {
            anchor: (0, col),
            cursor: (last_row, col),
        };
        let full = (0..=last_row).all(|row| self.is_selected(row, col));
        if full {
            self.remove_selection(whole);
        } else {
            self.add_selection(whole);
        }
        true
    }

    /// Appends a row of defaults and reveals it (the result bar's "Add row").
    pub fn add_row(&mut self) {
        let Some(edits) = &mut self.edits else {
            return;
        };
        let before = edits.clone();
        let row = edits.add_row();
        self.record_edits(before);
        self.reveal = Some(row);
        self.actions.push(GridAction::AddRow);
    }

    /// Sets a cell through the edit buffer (the value panel's editor saves here).
    pub fn set_cell(
        &mut self,
        rows: &RowBuffer,
        row: usize,
        col: usize,
        value: Value,
    ) -> Result<(), String> {
        let edits = self.edits.as_mut().ok_or("The result cannot be edited")?;
        let before = edits.clone();
        edits.set(rows, row, col, value.clone())?;
        self.record_edits(before);
        self.actions.push(GridAction::SetCell { row, col, value });
        Ok(())
    }

    fn refresh_view(&mut self, rows: &RowBuffer, local_sort: bool) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for c in rows.columns() {
            c.name.hash(&mut h);
            c.type_name.hash(&mut h);
        }
        let key = ViewKey {
            loaded: rows.row_count(),
            revision: rows.revision(),
            inserted: self
                .edits
                .as_ref()
                .map_or_else(Vec::new, EditBuffer::inserted_row_numbers),
            sort: if local_sort {
                self.sort.clone()
            } else {
                Vec::new()
            },
            columns: h.finish(),
        };
        if let Some((done_key, order)) = self.sort_worker.as_ref().and_then(|w| w.take())
            && done_key == key
        {
            self.view = Some((key.clone(), RowOrder::Sorted(order)));
            self.sorting = None;
            self.extra.clear();
        }
        if self.view.as_ref().is_some_and(|(old, _)| *old == key) && self.sorting.is_none() {
            return;
        }
        if key.sort.is_empty() {
            if self
                .view
                .as_ref()
                .is_some_and(|(old, _)| !old.sort.is_empty())
            {
                self.extra.clear();
            }
            self.sorting = None;
            if let Some(worker) = &self.sort_worker {
                worker.clear();
            }
            self.view = Some((
                key.clone(),
                RowOrder::Natural {
                    loaded: key.loaded,
                    inserted: key.inserted,
                },
            ));
        } else if key.loaded >= sort_worker::BACKGROUND_SORT_ROWS {
            if self.sorting.as_ref() != Some(&key) {
                let worker = self
                    .sort_worker
                    .get_or_insert_with(|| sort_worker::SortWorker::new(self.sort_repaint.clone()));
                worker.request(key.clone(), rows.clone());
                self.sorting = Some(key.clone());
                // Keep a valid view while the worker computes its replacement.
                if self
                    .view
                    .as_ref()
                    .is_none_or(|(old, _)| old.loaded != key.loaded || old.columns != key.columns)
                {
                    self.view = Some((
                        key.clone(),
                        RowOrder::Natural {
                            loaded: key.loaded,
                            inserted: key.inserted.clone(),
                        },
                    ));
                }
            }
        } else {
            self.sorting = None;
            let view = sort::compute(rows, &key.sort, true, &key.inserted);
            self.extra.clear();
            self.view = Some((key, RowOrder::Sorted(view)));
        }
    }

    /// Whether the worker is computing a local sort. The old order remains usable.
    pub fn is_sorting(&self) -> bool {
        self.sorting.is_some()
    }

    fn view_len(&self) -> usize {
        self.view.as_ref().map_or(0, |(_, v)| v.len())
    }

    fn source(&self, view_row: usize) -> Option<usize> {
        self.view.as_ref()?.1.get(view_row)
    }

    fn view_row_of(&self, source: usize) -> Option<usize> {
        self.view.as_ref()?.1.position(source)
    }

    fn select(&mut self, selection: Selection) -> bool {
        let changed =
            self.selected.is_none() || self.selection != selection || !self.extra.is_empty();
        self.selection = selection;
        self.extra.clear();
        self.sync_selected();
        changed
    }

    /// Marks every selected row deleted, newest first so inserted rows can go too.
    fn delete_selected(&mut self) {
        let selected = self.selected_rows();
        if selected.is_empty() {
            return;
        }
        let Some(edits) = &mut self.edits else {
            return;
        };
        let before = edits.clone();
        let mut doomed = selected.clone();
        doomed.sort_unstable_by(|a, b| b.cmp(a));
        for &row in &doomed {
            edits.delete(row);
        }
        self.record_edits(before);
        self.actions.push(GridAction::DeleteRows(selected));
    }

    fn sync_selected(&mut self) {
        let (r, c) = self.selection.cursor;
        self.selected = self.source(r).map(|s| (s, c));
    }

    /// The cell's current value: pending edit, inserted value (`None`: default) or loaded.
    fn value(&self, rows: &RowBuffer, row: usize, col: usize) -> Option<Value> {
        match &self.edits {
            Some(e) => e.value(rows, row, col),
            None => rows.cell(row, col).map(Value::from_cell),
        }
    }
}

/// First line only in spirit: newlines become "⏎", so a cell is always one line, cut at
/// [`MAX_CELL_CHARS`] chars plus "…". Counts chars, never bytes. Bytes show as `0x` plus the
/// hex of the first [`MAX_CELL_BYTES`], "…" and their count.
pub fn cell_display(cell: Cell<'_>) -> Cow<'_, str> {
    match cell {
        Cell::Text(s) => text_display(s),
        Cell::Bytes(b) => {
            let shown = sqlr_core::hex_bytes(&b[..b.len().min(MAX_CELL_BYTES)]);
            let unit = if b.len() == 1 { "byte" } else { "bytes" };
            Cow::Owned(format!("{shown}… ({} {unit})", b.len()))
        }
        other => Cow::Owned(other.to_string()),
    }
}

/// The display formats the app set for the grids; `None` for the defaults.
pub fn data_formats(ctx: &egui::Context) -> Option<Arc<DataFormats>> {
    ctx.data(|d| d.get_temp::<Option<Arc<DataFormats>>>(egui::Id::new("sqlr_data_formats")))
        .flatten()
}

/// Sets the display formats every grid of the context applies. Formats that change nothing
/// leave the values as the server sent them.
pub fn set_data_formats(ctx: &egui::Context, formats: DataFormats) {
    let formats = (!formats.is_default()).then(|| Arc::new(formats));
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("sqlr_data_formats"), formats));
}

fn null_text(formats: Option<&DataFormats>) -> Cow<'static, str> {
    match formats {
        Some(f) => Cow::Owned(f.null_text.clone()),
        None => Cow::Borrowed(NULL_TEXT),
    }
}

/// Multi-line JSON up to this size is re-serialized on one line; it is parsed every frame
/// the cell is visible, so larger values only get `⏎` for their line breaks.
const COMPACT_JSON_BYTES: usize = 4096;

/// A cell as the grid shows it: [`cell_display`], with JSON compacted to one line.
pub(crate) fn grid_text<'a>(
    formats: Option<&DataFormats>,
    meta: &ColumnMeta,
    cell: Cell<'a>,
) -> Cow<'a, str> {
    if let Some(formats) = formats {
        return Cow::Owned(text_display(&formats.format(meta, cell)).into_owned());
    }
    if let (DisplayHint::Json, Cell::Text(raw)) = (meta.hint, cell)
        && raw.len() <= COMPACT_JSON_BYTES
        && raw.contains(['\n', '\r'])
        && let Some(compact) = sqlr_core::compact_json(raw)
    {
        return Cow::Owned(text_display(&compact).into_owned());
    }
    cell_display(cell)
}

fn text_display(s: &str) -> Cow<'_, str> {
    // Each char is at least one byte, so short strings are within the limit.
    if s.len() <= MAX_CELL_CHARS && !s.contains(['\n', '\r']) {
        return Cow::Borrowed(s);
    }
    let mut out = String::new();
    let mut chars = 0;
    for c in s.chars() {
        let mapped = match c {
            '\n' => '⏎',
            '\r' => continue,
            c => c,
        };
        if chars == MAX_CELL_CHARS {
            out.push('…');
            return Cow::Owned(out);
        }
        out.push(mapped);
        chars += 1;
    }
    Cow::Owned(out)
}

fn is_right_aligned(meta: &ColumnMeta) -> bool {
    matches!(meta.kind, ColumnKind::Int | ColumnKind::Float)
        || matches!(meta.hint, DisplayHint::Number | DisplayHint::Decimal)
}

fn layout_key(
    columns: &[ColumnMeta],
    sampled_rows: usize,
    glyph_width: f32,
    formats: Option<&DataFormats>,
) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    formats.map(DataFormats::revision).hash(&mut h);
    for c in columns {
        c.name.hash(&mut h);
        c.type_name.hash(&mut h);
    }
    sampled_rows.hash(&mut h);
    glyph_width.to_bits().hash(&mut h);
    h.finish()
}

fn measure_columns(rows: &RowBuffer, glyph_width: f32, formats: Option<&DataFormats>) -> Vec<f32> {
    let columns = rows.columns();
    let sampled = rows.row_count().min(WIDTH_SAMPLE_ROWS);
    let mut x = Vec::with_capacity(columns.len() + 1);
    let mut left = 0.0;
    x.push(left);
    for (col, meta) in columns.iter().enumerate() {
        // Room for the sort arrow next to the name.
        let mut chars = (meta.name.chars().count() + 2).max(meta.type_name.chars().count());
        for row in 0..sampled {
            if let Some(cell) = rows.cell(row, col) {
                chars = chars.max(grid_text(formats, meta, cell).chars().count());
            }
        }
        let width =
            (chars as f32 * glyph_width + 2.0 * CELL_PADDING).clamp(MIN_COL_WIDTH, MAX_COL_WIDTH);
        left += width;
        x.push(left);
    }
    x
}

/// The grid of `rows` without editing, filters or literals.
pub fn result_grid(
    ui: &mut egui::Ui,
    state: &mut GridState,
    rows: &RowBuffer,
    more_available: bool,
) -> GridOutput {
    data_grid(
        ui,
        state,
        &GridInput {
            rows,
            more_available,
            dialect: None,
            table: None,
        },
    )
}

/// A context-menu choice, applied once the menu is drawn.
enum MenuChoice {
    Undo,
    Redo,
    CopyAll,
    Paste,
    PasteRows,
    RevertCells,
    Copy(CopyFormat),
    Edit,
    SetNull,
    Duplicate,
    AddRow,
    Delete,
    Revert,
    OpenValue,
    FilterByValue,
}

pub fn data_grid(ui: &mut egui::Ui, state: &mut GridState, input: &GridInput<'_>) -> GridOutput {
    let mut output = GridOutput {
        need_more: false,
        selection_changed: false,
    };
    let rows = input.rows;
    let columns = rows.columns();
    if columns.is_empty() {
        return output;
    }
    filter_bar(ui, state);
    server_sort_bar(ui, state);
    state.sort_repaint = Some(ui.ctx().clone());
    state.refresh_view(rows, !input.more_available);
    if state.is_sorting() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Sorting…");
        });
    }
    let view_len = state.view_len();
    if let Some(source) = state.reveal.take()
        && let Some(row) = state.view_row_of(source)
    {
        state.selected = Some((source, 0));
        state.selection = Selection::cell(row, 0);
        state.scroll_to_row = Some(row);
        state.refocus = true;
    }
    if view_len == 0 {
        state.selected = None;
    } else if state.selected.is_some() {
        state.selection.clamp(view_len, columns.len());
        state
            .extra
            .retain(|area| *area.rows().start() < view_len && *area.cols().start() < columns.len());
        for area in &mut state.extra {
            area.clamp(view_len, columns.len());
        }
        state.sync_selected();
    }
    if view_len == 0 && state.edits.is_some() {
        ui.horizontal(|ui| {
            ui.label("No rows");
            if ui.button("Paste as new rows…").clicked() {
                state.paste_new_rows = true;
                state.refocus = true;
                crate::keymap::request_paste(ui.ctx());
            }
        });
    }
    if state
        .editor
        .as_ref()
        .is_some_and(|e| state.view_row_of(e.row).is_none() || e.col >= columns.len())
    {
        state.editor = None;
    }

    let font = theme::monospace(ui.ctx());
    let palette = theme::palette(ui.ctx());
    let name_font = theme::semibold(ui.ctx(), 13.0);
    let type_font = theme::small_monospace(ui.ctx());
    let glyph_width = ui.fonts_mut(|f| f.glyph_width(&font, 'M'));
    let formats = data_formats(ui.ctx());
    let key = layout_key(
        columns,
        rows.row_count().min(WIDTH_SAMPLE_ROWS),
        glyph_width,
        formats.as_deref(),
    );
    if state.layout.as_ref().is_none_or(|l| l.key != key) {
        state.layout = Some(ColumnLayout {
            key,
            x: measure_columns(rows, glyph_width, formats.as_deref()),
        });
    }
    let col_x: Vec<f32> = state
        .layout
        .as_ref()
        .map(|l| l.x.clone())
        .unwrap_or_default();
    let digits = view_len.max(1).to_string().len() as f32;
    let gutter = (digits * glyph_width + 2.0 * CELL_PADDING).max(MIN_GUTTER);

    let mut area = egui::ScrollArea::both()
        .id_salt("result_grid_scroll")
        .auto_shrink(false);
    if let Some(row) = state.scroll_to_row.take() {
        area = area.vertical_scroll_offset(row as f32 * ROW_HEIGHT);
    }
    let total_width = gutter + col_x.last().copied().unwrap_or(0.0);
    let total_height = HEADER_HEIGHT + view_len as f32 * ROW_HEIGHT;

    area.show_viewport(ui, |ui, viewport| {
        let (rect, response) =
            ui.allocate_exact_size(vec2(total_width, total_height), Sense::click_and_drag());
        let geo = Geometry {
            origin: rect.min,
            viewport,
            gutter,
            col_x: &col_x,
            row_height: ROW_HEIGHT,
            view_rows: view_len,
        };
        // Row range inside the viewport (rows hidden under the sticky header excluded).
        let first_visible = ((viewport.min.y / ROW_HEIGHT).floor().max(0.0) as usize).min(view_len);
        let last_visible = (((viewport.max.y - HEADER_HEIGHT) / ROW_HEIGHT)
            .ceil()
            .max(0.0) as usize)
            .min(view_len)
            .max(first_visible);
        state.last_visible_rows = first_visible..last_visible;

        if state.refocus && state.editor.is_none() {
            response.request_focus();
            state.refocus = false;
        }
        output.selection_changed |= pointer(ui, state, input, &response, &geo);
        if response.has_focus() && state.editor.is_none() {
            output.selection_changed |= keyboard(ui, response.id, state, input, &geo);
        }

        paint_rows(
            ui,
            state,
            input,
            &geo,
            &response,
            (&font, first_visible, last_visible),
        );
        let first_col = col_x
            .partition_point(|&x| x <= viewport.min.x)
            .saturating_sub(1);
        let last_col = col_x
            .partition_point(|&x| x < viewport.max.x)
            .min(columns.len());
        header::paint(
            ui,
            response.id,
            &geo,
            columns,
            first_col..last_col,
            &state.sort,
            &palette,
            (&name_font, &type_font),
        );
        cell_editor(ui, state, input, &geo, &font);

        let mut choice = None;
        theme::context_menu(&response, |ui| {
            choice = context_menu(ui, state, input.dialect.is_some() || input.table.is_some())
        });
        if let Some(choice) = choice {
            apply_menu(ui, state, input, choice);
        }
    });

    clipboard::mapping_ui(ui, state, input);
    output.need_more =
        input.more_available && state.last_visible_rows.end + NEED_MORE_MARGIN >= rows.row_count();
    output
}

/// The server `WHERE` field: re-runs the query with the condition on ↵.
fn filter_bar(ui: &mut egui::Ui, state: &mut GridState) {
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(Spacing::M as i8, Spacing::S as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = Spacing::S;
                icons::paint(ui, icons::FILTER, theme::palette(ui.ctx()).text_muted);
                let field = crate::text_menu::text_edit(
                    ui,
                    TextEdit::singleline(&mut state.where_filter)
                        .hint_text("condition, ↵ to re-run")
                        .desired_width(f32::INFINITY),
                );
                if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    state.actions.push(GridAction::FilterOnServer(
                        state.where_filter.trim().to_string(),
                    ));
                }
            });
        });
}

/// "Sort on server?" after a header click on a partial result.
fn server_sort_bar(ui: &mut egui::Ui, state: &mut GridState) {
    let Some(order) = state.server_sort.clone() else {
        return;
    };
    ui.horizontal(|ui| {
        let muted = theme::palette(ui.ctx()).text_muted;
        ui.label(RichText::new("Not every row is loaded: sorting needs the server.").color(muted));
        if ui.button("Sort on server").clicked() {
            state.actions.push(GridAction::SortOnServer(order));
            state.server_sort = None;
        }
        if ui.button("Cancel").clicked() {
            state.server_sort = None;
        }
    });
}

/// Clicks, drags and right-clicks; returns whether the selection changed.
fn pointer(
    ui: &egui::Ui,
    state: &mut GridState,
    input: &GridInput<'_>,
    response: &egui::Response,
    geo: &Geometry<'_>,
) -> bool {
    let any = response.clicked()
        || response.secondary_clicked()
        || response.double_clicked()
        || response.drag_started();
    if any {
        response.request_focus();
    }
    let Some(pos) = response.interact_pointer_pos() else {
        return false;
    };
    let Some(hit) = geo.hit(pos) else {
        return false;
    };
    let (shift, command) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
    let last_col = geo.columns().saturating_sub(1);
    let last_row = geo.view_rows.saturating_sub(1);
    let anchor = state.selection.anchor;
    let has = state.selected.is_some();
    if response.double_clicked() {
        if let Hit::Cell(r, c) = hit {
            let changed = state.select(Selection::cell(r, c));
            open_editor(state, input, r, c);
            return changed;
        }
        return false;
    }
    if response.clicked() {
        if command && geo.view_rows > 0 {
            match hit {
                Hit::Cell(r, c) => return state.toggle_cell(r, c),
                Hit::Gutter(r) => return state.toggle_row(r, last_col),
                Hit::Header(c) => return state.toggle_column(c, last_row),
                Hit::Corner => return false,
            }
        }
        let next = match hit {
            Hit::Cell(r, c) if shift && has => Selection {
                anchor,
                cursor: (r, c),
            },
            Hit::Cell(r, c) => Selection::cell(r, c),
            Hit::Gutter(r) if shift && has => Selection {
                anchor: (anchor.0, 0),
                cursor: (r, last_col),
            },
            Hit::Gutter(r) => Selection {
                anchor: (r, 0),
                cursor: (r, last_col),
            },
            Hit::Header(c) if shift => {
                if geo.view_rows == 0 {
                    return false;
                }
                Selection {
                    anchor: (0, c),
                    cursor: (last_row, c),
                }
            }
            Hit::Header(c) => {
                let next = sort::cycle(&state.sort, c);
                if input.more_available {
                    state.server_sort = Some(next);
                } else {
                    state.sort = next.clone();
                    state.actions.push(GridAction::Sort(next));
                }
                return false;
            }
            Hit::Corner => {
                if geo.view_rows == 0 {
                    return false;
                }
                Selection {
                    anchor: (0, 0),
                    cursor: (last_row, last_col),
                }
            }
        };
        return state.select(next);
    }
    if response.secondary_clicked() {
        let (r, c, whole_row) = match hit {
            Hit::Cell(r, c) => (r, c, false),
            Hit::Gutter(r) => (r, 0, true),
            _ => return false,
        };
        if state.is_selected(r, c) {
            return false;
        }
        return state.select(if whole_row {
            Selection {
                anchor: (r, 0),
                cursor: (r, last_col),
            }
        } else {
            Selection::cell(r, c)
        });
    }
    if response.drag_started()
        && let Hit::Cell(r, c) = hit
    {
        return state.select(Selection::cell(r, c));
    }
    if response.dragged()
        && has
        && let Hit::Cell(r, c) = hit
    {
        return state.select(Selection {
            anchor,
            cursor: (r, c),
        });
    }
    false
}

/// Arrow keys, Home/End, PgUp/PgDn (with ⇧ to extend), ⌘A, ↵ to edit and copy; returns
/// whether the selection changed.
fn keyboard(
    ui: &egui::Ui,
    id: egui::Id,
    state: &mut GridState,
    input: &GridInput<'_>,
    geo: &Geometry<'_>,
) -> bool {
    let rows = geo.view_rows;
    let cols = geo.columns();
    if cols == 0 {
        return false;
    }
    ui.memory_mut(|m| {
        m.set_focus_lock_filter(
            id,
            egui::EventFilter {
                horizontal_arrows: true,
                vertical_arrows: true,
                ..Default::default()
            },
        )
    });
    let page = state.last_visible_rows.len().max(1) as isize;
    let keymap = crate::keymap::ui_keymap(ui.ctx());
    let (step, extend, all, edit, copy, paste, delete) = ui.input_mut(|i| {
        let mut step = None;
        let mut extend = false;
        let keys = [
            (Key::ArrowUp, (-1isize, 0isize)),
            (Key::ArrowDown, (1, 0)),
            (Key::ArrowLeft, (0, -1)),
            (Key::ArrowRight, (0, 1)),
            (Key::PageUp, (-page, 0)),
            (Key::PageDown, (page, 0)),
            (Key::Home, (0, isize::MIN / 2)),
            (Key::End, (0, isize::MAX / 2)),
        ];
        for (key, delta) in keys {
            if i.consume_key(Modifiers::SHIFT, key) {
                step = Some(delta);
                extend = true;
            } else if i.consume_key(Modifiers::NONE, key) {
                step = Some(delta);
            }
        }
        let all = keymap.consume(i, sqlr_core::ActionId::SelectAll);
        let edit = i.consume_key(Modifiers::NONE, Key::Enter);
        let copy = i.events.iter().any(|e| matches!(e, Event::Copy));
        let paste = i.events.iter().find_map(|e| match e {
            Event::Paste(text) => Some(text.clone()),
            _ => None,
        });
        if paste.is_some() {
            i.events.retain(|e| !matches!(e, Event::Paste(_)));
        }
        let delete = keymap.consume(i, sqlr_core::ActionId::DeleteRows);
        (step, extend, all, edit, copy, paste, delete)
    });
    let (undo, redo) = ui.input_mut(|i| {
        (
            keymap.consume(i, sqlr_core::ActionId::Undo),
            keymap.consume(i, sqlr_core::ActionId::Redo),
        )
    });
    if undo {
        state.undo();
    }
    if redo {
        state.redo();
    }
    let mut changed = false;
    if all && rows > 0 {
        changed |= state.select(Selection {
            anchor: (0, 0),
            cursor: (rows - 1, cols - 1),
        });
    }
    if let Some((dr, dc)) = step.filter(|_| rows > 0) {
        let has = state.selected.is_some();
        let (r, c) = if has { state.selection.cursor } else { (0, 0) };
        let next = (
            r.saturating_add_signed(dr).min(rows - 1),
            c.saturating_add_signed(dc).min(cols - 1),
        );
        let selection = if extend && has {
            Selection {
                anchor: state.selection.anchor,
                cursor: next,
            }
        } else {
            Selection::cell(next.0, next.1)
        };
        changed |= state.select(selection);
        let cell = geo.cell_rect(next.0, next.1);
        ui.scroll_to_rect(
            Rect::from_min_max(cell.min - vec2(geo.gutter, HEADER_HEIGHT), cell.max),
            None,
        );
    }
    if edit && state.selected.is_some() {
        let (r, c) = state.selection.cursor;
        open_editor(state, input, r, c);
    }
    if copy && state.selected.is_some() {
        copy_selection(ui.ctx(), state, input, CopyFormat::Tsv);
    }
    if let Some(text) = paste {
        if std::mem::take(&mut state.paste_new_rows) {
            clipboard::begin_paste_rows(ui.ctx(), state, input, &text);
        } else {
            clipboard::paste_text(ui.ctx(), state, input, &text);
        }
    }
    if delete {
        state.delete_selected();
    }
    changed
}

/// Copies the selection in `format`, or asks the caller to when it is large.
fn copy_selection(
    ctx: &egui::Context,
    state: &mut GridState,
    input: &GridInput<'_>,
    format: CopyFormat,
) {
    let cells = state
        .selected_view_rows()
        .len()
        .saturating_mul(state.selected_cols().len());
    let source = state.clipboard_source(input.rows);
    let estimate = input.rows.byte_size() as f64 * cells as f64
        / (input
            .rows
            .row_count()
            .saturating_mul(input.rows.columns().len()))
        .max(1) as f64;
    if cells > MAX_COPY_CELLS || estimate > MAX_COPY_BYTES as f64 {
        state.actions.push(GridAction::Copy(format));
        return;
    }
    match source.encode(
        format.text_format(state.whole_columns(), input.table),
        input.dialect,
        |_, _| true,
    ) {
        Ok(Some(mut data)) => {
            if format == CopyFormat::Tsv
                && state.selected_cells() == 1
                && let Some(value) = data
                    .rows
                    .first()
                    .and_then(|r| r.first())
                    .and_then(Option::as_ref)
            {
                data.text = value.display().into_owned();
            }
            clipboard::set_grid_clipboard(ctx, Arc::new(data));
        }
        Ok(None) => {}
        Err(error) => state.actions.push(GridAction::PasteFailed(error)),
    }
}

/// Opens the inline editor on a view cell, or asks for the value panel when the value is
/// multi-line, long, JSON, XML or binary. Ignored when the cell cannot be edited.
fn open_editor(state: &mut GridState, input: &GridInput<'_>, view_row: usize, col: usize) {
    let Some(row) = state.source(view_row) else {
        return;
    };
    let Some(edits) = &state.edits else {
        return;
    };
    if !edits.writable(col) || edits.row_state(row) == RowState::Deleted {
        return;
    }
    let meta = &input.rows.columns()[col];
    let value = edits.value(input.rows, row, col).unwrap_or(Value::Null);
    let panel = match &value {
        Value::Text(t) => t.contains(['\n', '\r']) || t.chars().count() > INLINE_EDIT_CHARS,
        Value::Bytes(_) => true,
        _ => false,
    } || matches!(meta.hint, DisplayHint::Json | DisplayHint::Xml)
        || meta.kind == ColumnKind::Bytes;
    if panel {
        state.actions.push(GridAction::EditCell { row, col });
        return;
    }
    state.editor = Some(CellEditor {
        row,
        col,
        text: edit::edit_text(&value),
        error: None,
        focus: true,
        select_all: false,
    });
}

/// Draws the inline editor and commits (↵, ⇥ or a click elsewhere) or cancels (Esc) it.
fn cell_editor(
    ui: &mut egui::Ui,
    state: &mut GridState,
    input: &GridInput<'_>,
    geo: &Geometry<'_>,
    font: &egui::FontId,
) {
    let Some(ed) = state.editor.as_ref() else {
        return;
    };
    let Some(view_row) = state.view_row_of(ed.row) else {
        return;
    };
    let palette = theme::palette(ui.ctx());
    let rect = geo.cell_rect(view_row, ed.col);
    let id = ui.id().with("grid_cell_editor");
    let Some(ed) = state.editor.as_mut() else {
        return;
    };
    let mut edit = TextEdit::singleline(&mut ed.text)
        .id(id)
        .font(font.clone())
        .margin(vec2(CELL_PADDING - 2.0, 2.0))
        .desired_width(rect.width());
    if ed.error.is_some() {
        edit = edit.background_color(change_tint(palette.error));
    }
    let response = ui.put(rect, edit);
    crate::text_menu::text_edit_menu(&response, crate::text_menu::TextMenuMode::Editable);
    if ed.focus {
        response.request_focus();
        ed.focus = false;
        ed.select_all = true;
    }
    if ed.select_all {
        // Once focused, the whole value is selected so typing replaces it.
        let mut te = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
        te.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(0),
            CCursor::new(ed.text.chars().count()),
        )));
        te.store(ui.ctx(), id);
        // Kept until the editor holds the focus, so text typed right away replaces it.
        ed.select_all = !response.has_focus();
    }
    if let Some(error) = &ed.error {
        let below = Rect::from_min_size(rect.left_bottom(), vec2(rect.width().max(160.0), 20.0));
        ui.put(
            below,
            egui::Label::new(RichText::new(error).color(palette.error)),
        );
    }
    if !response.lost_focus() {
        return;
    }
    let (escape, tab) = ui.input(|i| (i.key_pressed(Key::Escape), i.key_pressed(Key::Tab)));
    let (row, col) = (ed.row, ed.col);
    if escape {
        state.editor = None;
        state.refocus = true;
        return;
    }
    let meta = &input.rows.columns()[col];
    let parsed = parse_input(meta, &ed.text);
    let outcome = parsed.and_then(|value| {
        let edits = state.edits.as_mut().ok_or("The result cannot be edited")?;
        edits.set(input.rows, row, col, value.clone())?;
        Ok(value)
    });
    match outcome {
        Ok(value) => {
            state.actions.push(GridAction::SetCell { row, col, value });
            state.editor = None;
            state.refocus = true;
            if tab && col + 1 < geo.columns() {
                state.select(Selection::cell(view_row, col + 1));
            }
        }
        Err(error) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.error = Some(error);
                ed.focus = true;
            }
        }
    }
}

/// Paints the visible rows: zebra stripes, change tints, the selection, cell text and the
/// row numbers.
fn paint_rows(
    ui: &egui::Ui,
    state: &GridState,
    input: &GridInput<'_>,
    geo: &Geometry<'_>,
    response: &egui::Response,
    (font, first_visible, last_visible): (&egui::FontId, usize, usize),
) {
    let rows = input.rows;
    let columns = rows.columns();
    let formats = data_formats(ui.ctx());
    let formats = formats.as_deref();
    let palette = theme::palette(ui.ctx());
    let painter = ui.painter().clone();
    let viewport = geo.viewport;
    painter.rect_filled(
        Rect::from_min_size(geo.origin + viewport.min.to_vec2(), viewport.size()),
        0.0,
        palette.bg_editor,
    );
    let Some((_, view)) = &state.view else {
        return;
    };
    let first_col = geo
        .col_x
        .partition_point(|&x| x <= viewport.min.x)
        .saturating_sub(1);
    let last_col = geo
        .col_x
        .partition_point(|&x| x < viewport.max.x - geo.gutter)
        .min(columns.len());
    let paint_rows =
        first_visible.saturating_sub(OVERSCAN_ROWS)..(last_visible + OVERSCAN_ROWS).min(view.len());
    let zebra = palette.zebra();
    let total_width = geo.gutter + geo.col_x.last().copied().unwrap_or(0.0);
    let editing = state.editor.as_ref().map(|e| (e.row, e.col));
    for vr in paint_rows {
        let src = view.get(vr).expect("visible row");
        let y = geo.origin.y + HEADER_HEIGHT + vr as f32 * ROW_HEIGHT;
        let row_rect = Rect::from_min_max(
            pos2(geo.origin.x, y),
            pos2(geo.origin.x + total_width, y + ROW_HEIGHT),
        );
        if vr % 2 == 1 {
            painter.rect_filled(row_rect, 0.0, zebra);
        }
        let row_state = state
            .edits
            .as_ref()
            .map_or(RowState::Clean, |e| e.row_state(src));
        match row_state {
            RowState::Inserted => {
                painter.rect_filled(row_rect, 0.0, change_tint(palette.success));
            }
            RowState::Deleted => {
                painter.rect_filled(row_rect, 0.0, change_tint(palette.error));
            }
            _ => {}
        }
        for (col, meta) in columns.iter().enumerate().take(last_col).skip(first_col) {
            let rect = geo.cell_rect(vr, col);
            if state.is_selected(vr, col) {
                painter.rect_filled(rect, 0.0, palette.selection);
            }
            let edited = state.edits.as_ref().is_some_and(|e| e.is_edited(src, col));
            if edited {
                painter.rect_filled(rect, 0.0, change_tint(palette.warning));
            }
            if editing == Some((src, col)) {
                continue;
            }
            // Only pending values are owned; every other cell is read from its chunk.
            let pending = match (&state.edits, row_state) {
                (Some(e), RowState::Inserted) => Some(e.value(rows, src, col)),
                (Some(e), _) if edited => Some(e.value(rows, src, col)),
                _ => None,
            };
            let cell = match &pending {
                Some(value) => value.as_ref().map(Value::as_cell),
                None => rows.cell(src, col),
            };
            let (text, color, italics) = match cell {
                None if row_state == RowState::Inserted => {
                    (Cow::Borrowed(DEFAULT_TEXT), palette.null_text, true)
                }
                None | Some(Cell::Null) => (null_text(formats), palette.null_text, true),
                Some(cell) => (grid_text(formats, meta, cell), palette.text, false),
            };
            let mut job = LayoutJob::default();
            job.append(
                &text,
                0.0,
                egui::TextFormat {
                    font_id: font.clone(),
                    color,
                    italics,
                    strikethrough: if row_state == RowState::Deleted {
                        Stroke::new(1.0, palette.error)
                    } else {
                        Stroke::NONE
                    },
                    ..Default::default()
                },
            );
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            let x = if is_right_aligned(meta) && !italics {
                rect.right() - CELL_PADDING - galley.size().x
            } else {
                rect.left() + CELL_PADDING
            };
            let pos = pos2(x, rect.center().y - galley.size().y / 2.0);
            let clipped = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
            clipped.galley(pos, galley, palette.text);
            // Painted text is invisible to screen readers (and tests) without a node.
            ui.ctx()
                .accesskit_node_builder(response.id.with((vr, col)), |node| {
                    node.set_role(egui::accesskit::Role::Label);
                    node.set_value(text.as_ref());
                    node.set_bounds(header::bounds(rect));
                });
        }
        // Row numbers, pinned to the left edge.
        let number = geo.gutter_rect(vr);
        painter.rect_filled(number, 0.0, palette.bg_panel);
        painter.vline(
            number.right(),
            number.y_range(),
            Stroke::new(1.0, palette.border),
        );
        let label = (vr + 1).to_string();
        painter.text(
            pos2(number.right() - CELL_PADDING, number.center().y),
            egui::Align2::RIGHT_CENTER,
            &label,
            font.clone(),
            palette.text_muted,
        );
        ui.ctx()
            .accesskit_node_builder(response.id.with(("row", vr)), |node| {
                node.set_role(egui::accesskit::Role::RowHeader);
                node.set_label(format!("Row {label}"));
                node.set_bounds(header::bounds(number));
            });
    }
}

/// A context-menu button; closes the menu when clicked.
fn menu_item(ui: &mut egui::Ui, enabled: bool, icon: &str, label: &str) -> bool {
    let clicked = icons::menu_item_enabled(ui, enabled, icon, label).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

fn copy_format_icon(format: CopyFormat) -> &'static str {
    match format {
        CopyFormat::Tsv => icons::TABLE,
        CopyFormat::Csv => icons::CSV,
        CopyFormat::Json => icons::JSON,
        CopyFormat::SqlInsert => icons::SQL,
        CopyFormat::Markdown => icons::MARKDOWN,
    }
}

/// The context menu of the selection; returns the item clicked.
fn context_menu(ui: &mut egui::Ui, state: &GridState, allow_copy_all: bool) -> Option<MenuChoice> {
    let mut choice = None;
    if ui
        .add_enabled(state.can_undo(), egui::Button::new("Undo"))
        .clicked()
    {
        choice = Some(MenuChoice::Undo);
        ui.close();
    }
    if ui
        .add_enabled(state.can_redo(), egui::Button::new("Redo"))
        .clicked()
    {
        choice = Some(MenuChoice::Redo);
        ui.close();
    }
    ui.separator();
    if allow_copy_all && ui.button("Copy all results").clicked() {
        choice = Some(MenuChoice::CopyAll);
        ui.close();
    }
    if menu_item(ui, true, icons::COPY, "Copy") {
        choice = Some(MenuChoice::Copy(CopyFormat::Tsv));
    }
    icons::menu_button(ui, icons::COPY, "Copy as", |ui| {
        for format in CopyFormat::ALL {
            if menu_item(ui, true, copy_format_icon(format), format.label()) {
                choice = Some(MenuChoice::Copy(format));
            }
        }
    });
    if ui
        .add_enabled(state.edits.is_some(), egui::Button::new("Paste"))
        .clicked()
    {
        choice = Some(MenuChoice::Paste);
        ui.close();
    }
    if ui
        .add_enabled(
            state.edits.is_some(),
            egui::Button::new("Paste as new rows…"),
        )
        .clicked()
    {
        choice = Some(MenuChoice::PasteRows);
        ui.close();
    }
    ui.separator();
    let rows = state.selected_rows();
    let (_, col) = state.selection.cursor;
    let editable = state.edits.is_some();
    let writable = state.edits.as_ref().is_some_and(|e| e.writable(col));
    let not_null = state.edits.as_ref().is_some_and(|e| e.not_null(col));
    let plural = if rows.len() > 1 { "s" } else { "" };
    if menu_item(ui, writable, icons::EDIT, "Edit cell") {
        choice = Some(MenuChoice::Edit);
    }
    let can_null = state.edits.as_ref().is_some_and(|e| {
        state.selected.is_some()
            && state
                .selected_cols()
                .iter()
                .all(|&c| e.writable(c) && !e.not_null(c))
    });
    let set_null = icons::menu_item_enabled(ui, can_null, icons::NULL, "Set NULL")
        .on_disabled_hover_text(if not_null {
            "The column is NOT NULL"
        } else {
            "The cell cannot be edited"
        });
    if set_null.clicked() {
        choice = Some(MenuChoice::SetNull);
        ui.close();
    }
    if menu_item(
        ui,
        editable && !rows.is_empty(),
        icons::DUPLICATE,
        &format!("Duplicate row{plural}"),
    ) {
        choice = Some(MenuChoice::Duplicate);
    }
    if menu_item(ui, editable, icons::ADD, "Add row") {
        choice = Some(MenuChoice::AddRow);
    }
    if menu_item(
        ui,
        editable && !rows.is_empty(),
        icons::DELETE,
        &format!("Delete row{plural}"),
    ) {
        choice = Some(MenuChoice::Delete);
    }
    if ui
        .add_enabled(
            editable && state.selected.is_some(),
            egui::Button::new("Revert selected cells"),
        )
        .clicked()
    {
        choice = Some(MenuChoice::RevertCells);
        ui.close();
    }
    let changed = state
        .edits
        .as_ref()
        .is_some_and(|e| rows.iter().any(|&r| e.row_state(r) != RowState::Clean));
    if menu_item(ui, changed, icons::ROLLBACK, &format!("Revert row{plural}")) {
        choice = Some(MenuChoice::Revert);
    }
    ui.separator();
    if menu_item(ui, true, icons::PANEL, "Open in value panel") {
        choice = Some(MenuChoice::OpenValue);
    }
    if menu_item(ui, true, icons::FILTER, "Filter by this value") {
        choice = Some(MenuChoice::FilterByValue);
    }
    choice
}

fn apply_menu(ui: &egui::Ui, state: &mut GridState, input: &GridInput<'_>, choice: MenuChoice) {
    state.refocus = true;
    let rows = input.rows;
    let selected = state.selected_rows();
    match choice {
        MenuChoice::Undo => {
            state.undo();
            return;
        }
        MenuChoice::Redo => {
            state.redo();
            return;
        }
        MenuChoice::CopyAll => {
            state.actions.push(GridAction::CopyAll);
            return;
        }
        MenuChoice::Paste | MenuChoice::PasteRows => {
            state.paste_new_rows = matches!(choice, MenuChoice::PasteRows);
            crate::keymap::request_paste(ui.ctx());
            state.refocus = true;
            return;
        }
        MenuChoice::AddRow => {
            state.add_row();
            return;
        }
        _ => {}
    }
    let Some((row, col)) = state.selected else {
        return;
    };
    match choice {
        MenuChoice::Undo
        | MenuChoice::Redo
        | MenuChoice::CopyAll
        | MenuChoice::Paste
        | MenuChoice::PasteRows
        | MenuChoice::AddRow => {}
        MenuChoice::Copy(format) => copy_selection(ui.ctx(), state, input, format),
        MenuChoice::Edit => {
            let (vr, c) = state.selection.cursor;
            open_editor(state, input, vr, c);
        }
        MenuChoice::SetNull => state.set_selected_null(rows),
        MenuChoice::RevertCells => state.revert_selected_cells(),
        MenuChoice::Duplicate => {
            if let Some(edits) = &mut state.edits {
                let before = edits.clone();
                for &source in &selected {
                    edits.duplicate(rows, source);
                }
                state.record_edits(before);
                state
                    .actions
                    .extend(selected.into_iter().map(GridAction::DuplicateRow));
            }
        }
        MenuChoice::Delete => state.delete_selected(),
        MenuChoice::Revert => {
            if let Some(edits) = &mut state.edits {
                let before = edits.clone();
                let mut undone = selected.clone();
                undone.sort_unstable_by(|a, b| b.cmp(a));
                for &r in &undone {
                    edits.revert(r);
                }
                state.record_edits(before);
                state.actions.push(GridAction::RevertRows(selected));
            }
        }
        MenuChoice::OpenValue => state.actions.push(GridAction::OpenValue { row, col }),
        MenuChoice::FilterByValue => {
            let meta = &rows.columns()[col];
            let value = state.value(rows, row, col).unwrap_or(Value::Null);
            let name = match input.dialect {
                Some(d) => d.quote_ident(&meta.name),
                None => format!("\"{}\"", meta.name.replace('"', "\"\"")),
            };
            let condition = match (&value, input.dialect) {
                (Value::Null, _) => format!("{name} IS NULL"),
                (v, Some(d)) => format!("{name} = {}", sqlr_core::sql_literal(d, v)),
                (v, None) => format!("{name} = {}", ansi_literal(v)),
            };
            let current = state.where_filter.trim();
            state.where_filter = if current.is_empty() {
                condition
            } else {
                format!("{current} AND {condition}")
            };
            state
                .actions
                .push(GridAction::FilterOnServer(state.where_filter.clone()));
        }
    }
}

/// A literal without a dialect: ANSI strings and `X'…'` bytes.
fn ansi_literal(value: &Value) -> String {
    struct Ansi;
    impl Dialect for Ansi {
        fn quote_ident(&self, ident: &str) -> String {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
        fn select_all(&self, _node: &sqlr_core::Node, _limit: Option<usize>) -> Option<String> {
            None
        }
    }
    sqlr_core::sql_literal(&Ansi, value)
}

#[cfg(test)]
mod format_tests {
    use super::*;
    use sqlr_core::SettingValue;

    fn formats(pairs: &[(&str, SettingValue)]) -> DataFormats {
        DataFormats::from_values(|id| pairs.iter().find(|(k, _)| *k == id).map(|(_, v)| v.clone()))
    }

    #[test]
    fn natural_order_does_not_allocate_per_row() {
        let cols: Arc<[ColumnMeta]> = vec![ColumnMeta::new(
            "i",
            "int8",
            ColumnKind::Int,
            DisplayHint::Number,
        )]
        .into();
        let mut rows = RowBuffer::new(cols.clone());
        let mut b = sqlr_core::ChunkBuilder::new(cols);
        for i in 0..100_000 {
            b.push_row(&[Cell::Int(i)]).unwrap();
        }
        rows.push_chunk(b.finish());
        let mut state = GridState::default();
        state.refresh_view(&rows, true);
        assert_eq!(state.view.as_ref().unwrap().1.capacity(), 0);
        assert_eq!(state.source(99_999), Some(99_999));
        assert_eq!(state.view_row_of(99_999), Some(99_999));
    }

    #[test]
    fn background_sort_applies_only_the_current_request() {
        let cols: Arc<[ColumnMeta]> = vec![ColumnMeta::new(
            "i",
            "int8",
            ColumnKind::Int,
            DisplayHint::Number,
        )]
        .into();
        let mut rows = RowBuffer::new(cols.clone());
        let mut b = sqlr_core::ChunkBuilder::new(cols);
        for i in (0..20_000).rev() {
            b.push_row(&[Cell::Int(i)]).unwrap();
        }
        rows.push_chunk(b.finish());
        let mut state = GridState {
            sort: vec![(0, false)],
            ..Default::default()
        };
        state.refresh_view(&rows, true);
        assert!(state.is_sorting());
        state.sort = vec![(0, true)];
        state.refresh_view(&rows, true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while state.is_sorting() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
            state.refresh_view(&rows, true);
        }
        assert_eq!(state.source(0), Some(0));
        state.sort.clear();
        state.refresh_view(&rows, true);
        assert_eq!(state.view.as_ref().unwrap().1.capacity(), 0);
    }

    #[test]
    fn same_shape_new_result_rebuilds_the_permutation() {
        let cols: Arc<[ColumnMeta]> = vec![ColumnMeta::new(
            "i",
            "int8",
            ColumnKind::Int,
            DisplayHint::Number,
        )]
        .into();
        let make = |values: &[i64]| {
            let mut rows = RowBuffer::new(cols.clone());
            let mut b = sqlr_core::ChunkBuilder::new(cols.clone());
            for &v in values {
                b.push_row(&[Cell::Int(v)]).unwrap();
            }
            rows.push_chunk(b.finish());
            rows
        };
        let mut state = GridState {
            sort: vec![(0, false)],
            ..Default::default()
        };
        state.refresh_view(&make(&[2, 1]), true);
        assert_eq!(state.source(0), Some(1));
        state.refresh_view(&make(&[1, 2]), true);
        assert_eq!(state.source(0), Some(0));
    }

    #[test]
    fn grid_uses_data_formats() {
        let int = ColumnMeta::new("n", "int8", ColumnKind::Int, DisplayHint::Number);
        let ts = ColumnMeta::new("t", "timestamp", ColumnKind::Text, DisplayHint::Temporal);
        let f = formats(&[
            (
                "formats.number_grouping",
                SettingValue::Choice("comma".into()),
            ),
            ("formats.timestamp", SettingValue::Text("%d/%m/%Y".into())),
            ("grid.null_text", SettingValue::Text("∅".into())),
        ]);
        // Without formats the grid shows what the server sent.
        assert_eq!(grid_text(None, &int, Cell::Int(1234567)), "1234567");
        assert_eq!(grid_text(Some(&f), &int, Cell::Int(1234567)), "1,234,567");
        assert_eq!(
            grid_text(Some(&f), &ts, Cell::Text("2024-03-05 10:11:12")),
            "05/03/2024"
        );
        assert_eq!(null_text(Some(&f)), "∅");
        assert_eq!(null_text(None), "NULL");
        // Long values are still cut after formatting.
        let long = "x".repeat(MAX_CELL_CHARS + 50);
        let shown = grid_text(Some(&f), &ts, Cell::Text(&long));
        assert_eq!(shown.chars().count(), MAX_CELL_CHARS + 1);
    }

    #[test]
    fn formats_travel_through_the_context() {
        let ctx = egui::Context::default();
        assert!(data_formats(&ctx).is_none());
        set_data_formats(
            &ctx,
            formats(&[("formats.boolean", SettingValue::Choice("check".into()))]),
        );
        assert!(data_formats(&ctx).is_some());
        // Formats that change nothing clear them, keeping the grid's fast path.
        set_data_formats(&ctx, DataFormats::default());
        assert!(data_formats(&ctx).is_none());
    }
}
