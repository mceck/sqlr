//! Typed clipboard round trips, repeated block paste, and new-row mapping preview.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};

use egui::{ComboBox, Context, Id};
use sqlr_core::{
    ClipboardRegion, ClipboardSource, ColumnKind, ColumnMeta, CsvOptions, DisplayHint,
    GridClipboard, TextFormat, Value,
};

use super::{GridAction, GridInput, GridState, parse_input};

fn clipboard_id() -> Id {
    Id::new("sqlr_typed_grid_clipboard")
}

/// Publishes text to the native clipboard and keeps a typed counterpart within sqlr.
/// Incoming paste text must still match before the typed payload can be reused.
pub fn set_grid_clipboard(ctx: &Context, data: Arc<GridClipboard>) {
    ctx.copy_text(data.text.clone());
    ctx.data_mut(|d| d.insert_temp(clipboard_id(), data));
}

fn clipboard_data(ctx: &Context, text: &str) -> Result<(Arc<GridClipboard>, bool), String> {
    if let Some(data) = ctx.data(|d| d.get_temp::<Arc<GridClipboard>>(clipboard_id()))
        && data.text == text
    {
        return Ok((data, false));
    }
    let (_, rows) = sqlr_core::decode(
        &TextFormat::Csv(CsvOptions {
            header: false,
            ..CsvOptions::tsv()
        }),
        text,
    )?;
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let columns = (0..width)
        .map(|i| {
            ColumnMeta::new(
                format!("Column {}", i + 1),
                "text",
                ColumnKind::Mixed,
                DisplayHint::Plain,
            )
        })
        .collect();
    Ok((
        Arc::new(GridClipboard {
            columns,
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(Some).collect())
                .collect(),
            text: text.into(),
        }),
        true,
    ))
}

impl GridState {
    pub fn clipboard_source(&self, rows: &sqlr_core::RowBuffer) -> ClipboardSource {
        ClipboardSource {
            rows: rows.clone(),
            order: self
                .selected_view_rows()
                .into_iter()
                .filter_map(|v| Some((v, self.source(v)?)))
                .collect(),
            cols: self.selected_cols(),
            regions: self
                .all_selections()
                .map(|s| ClipboardRegion {
                    rows: s.rows(),
                    cols: s.cols(),
                })
                .collect(),
            overrides: self
                .edits
                .as_ref()
                .map(|e| e.clipboard_overrides())
                .unwrap_or_default(),
        }
    }
}

fn coerce(meta: &ColumnMeta, value: &Value) -> Result<Value, String> {
    if value.is_null() || meta.kind == ColumnKind::Mixed {
        return Ok(value.clone());
    }
    match (meta.kind, value) {
        (ColumnKind::Int, Value::Int(_))
        | (ColumnKind::Float, Value::Float(_))
        | (ColumnKind::Bool, Value::Bool(_))
        | (ColumnKind::Bytes, Value::Bytes(_))
        | (ColumnKind::Text, Value::Text(_)) => Ok(value.clone()),
        _ => parse_input(meta, &value.display()),
    }
}

pub(super) fn paste_text(ctx: &Context, state: &mut GridState, input: &GridInput<'_>, text: &str) {
    let Some(mut trial) = state.edits.clone() else {
        state.actions.push(GridAction::PasteFailed(
            "The result cannot be edited".into(),
        ));
        return;
    };
    if state.selected.is_none() {
        return;
    }
    let data = match clipboard_data(ctx, text) {
        Ok((data, _)) => data,
        Err(error) => {
            state.actions.push(GridAction::PasteFailed(error));
            return;
        }
    };
    let height = data.rows.len();
    let width = data.columns.len();
    if height == 0 || width == 0 {
        return;
    }
    let view = state.view_rows();
    let columns = input.rows.columns();
    let apply = |trial: &mut super::EditBuffer,
                 row: usize,
                 col: usize,
                 value: &Value|
     -> Result<(), String> {
        let value = coerce(&columns[col], value)?;
        trial.set(input.rows, view[row], col, value)
    };
    let result = (|| -> Result<(), String> {
        if height == 1 && width == 1 {
            if let Some(Some(value)) = data.rows[0].first() {
                for row in state.selected_view_rows() {
                    for col in state.selected_cols() {
                        if state.is_selected(row, col) {
                            apply(&mut trial, row, col, value)?;
                        }
                    }
                }
            }
        } else {
            let mut areas: Vec<_> = state.all_selections().collect();
            areas.sort_by_key(|area| (*area.rows().start(), *area.cols().start()));
            for area in areas {
                let r0 = *area.rows().start();
                let c0 = *area.cols().start();
                let tile = area.rows().count().is_multiple_of(height)
                    && area.cols().count().is_multiple_of(width);
                let target_rows = if tile { area.rows().count() } else { height };
                let target_cols = if tile { area.cols().count() } else { width };
                for dr in 0..target_rows.min(view.len().saturating_sub(r0)) {
                    for dc in 0..target_cols.min(columns.len().saturating_sub(c0)) {
                        if let Some(Some(value)) = data.rows[dr % height].get(dc % width) {
                            apply(&mut trial, r0 + dr, c0 + dc, value)?;
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => state.replace_edits(trial),
        Err(error) => state.actions.push(GridAction::PasteFailed(error)),
    }
}

pub(super) struct PasteRowsDialog {
    data: Arc<GridClipboard>,
    mapping: Vec<Option<usize>>,
    regenerate: bool,
    external: bool,
    header: bool,
    error: Option<String>,
    work: Option<PasteRowsWork>,
}

struct PasteRowsWork {
    receiver: mpsc::Receiver<Result<super::EditBuffer, String>>,
    cancelled: Arc<AtomicBool>,
    processed: Arc<AtomicUsize>,
    baseline: super::EditBuffer,
}

impl Drop for PasteRowsWork {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl PasteRowsDialog {
    fn new(data: Arc<GridClipboard>, external: bool, input: &GridInput<'_>) -> Self {
        let mapping = data
            .columns
            .iter()
            .map(|source| {
                if external {
                    None
                } else {
                    input
                        .rows
                        .columns()
                        .iter()
                        .position(|target| target.name == source.name)
                }
            })
            .collect();
        Self {
            data,
            mapping,
            regenerate: true,
            external,
            header: false,
            error: None,
            work: None,
        }
    }

    fn prepare(
        &self,
        mut trial: super::EditBuffer,
        rows: &sqlr_core::RowBuffer,
        mut progress: impl FnMut(usize) -> bool,
    ) -> Result<super::EditBuffer, String> {
        trial.check_insert_columns()?;
        let columns = rows.columns();
        let mut used = std::collections::BTreeSet::new();
        for col in self.mapping.iter().flatten().copied() {
            if !trial.writable(col) {
                continue;
            }
            if self.regenerate && trial.automatic(col) {
                continue;
            }
            if !used.insert(col) {
                return Err(format!("{} is mapped more than once", columns[col].name));
            }
        }
        for (col, meta) in columns.iter().enumerate() {
            if trial.writable(col)
                && trial.not_null(col)
                && !trial.has_default(col)
                && !used.contains(&col)
            {
                return Err(format!(
                    "Map the required column {} before pasting",
                    meta.name
                ));
            }
        }
        let records = self.data.rows.iter().skip(usize::from(self.header));
        let mut count = 0;
        for values in records {
            if !progress(count) {
                return Err("Paste cancelled".into());
            }
            let row = trial.add_row();
            for (source, target) in self.mapping.iter().enumerate() {
                let Some(col) = *target else {
                    continue;
                };
                if !trial.writable(col) || (self.regenerate && trial.automatic(col)) {
                    continue;
                }
                if let Some(Some(value)) = values.get(source) {
                    let value = coerce(&columns[col], value)
                        .map_err(|e| format!("Row {}, {}: {e}", count + 1, columns[col].name))?;
                    trial
                        .set(rows, row, col, value)
                        .map_err(|e| format!("Row {}, {}: {e}", count + 1, columns[col].name))?;
                }
            }
            for (col, meta) in columns.iter().enumerate() {
                if trial.writable(col)
                    && trial.not_null(col)
                    && !trial.has_default(col)
                    && trial.value(rows, row, col).is_none()
                {
                    return Err(format!(
                        "Row {} has no value for required column {}",
                        count + 1,
                        meta.name
                    ));
                }
            }
            count += 1;
        }
        Ok(trial)
    }
    fn append(&self, state: &mut GridState, input: &GridInput<'_>) -> Result<usize, String> {
        let trial = self.prepare(
            state.edits.clone().ok_or("The result cannot be edited")?,
            input.rows,
            |_| true,
        )?;
        let count = self.data.rows.len() - usize::from(self.header);
        state.replace_edits(trial);
        Ok(count)
    }

    fn start(&mut self, ctx: &Context, edits: &super::EditBuffer, rows: &sqlr_core::RowBuffer) {
        let plan = Self {
            data: self.data.clone(),
            mapping: self.mapping.clone(),
            regenerate: self.regenerate,
            external: self.external,
            header: self.header,
            error: None,
            work: None,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let processed = Arc::new(AtomicUsize::new(0));
        let (tx, receiver) = mpsc::channel();
        let work = PasteRowsWork {
            receiver,
            cancelled: cancelled.clone(),
            processed: processed.clone(),
            baseline: edits.clone(),
        };
        let trial = edits.clone();
        let rows = rows.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = plan.prepare(trial, &rows, |count| {
                processed.store(count, Ordering::Relaxed);
                if count.is_multiple_of(256) {
                    ctx.request_repaint();
                }
                !cancelled.load(Ordering::Relaxed)
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.work = Some(work);
    }
}

pub(super) fn begin_paste_rows(
    ctx: &Context,
    state: &mut GridState,
    input: &GridInput<'_>,
    text: &str,
) {
    match clipboard_data(ctx, text) {
        Ok((data, external)) if !data.rows.is_empty() => {
            let mut dialog = PasteRowsDialog::new(data, external, input);
            if let Some(edits) = &state.edits {
                for mapping in &mut dialog.mapping {
                    if mapping.is_some_and(|col| !edits.writable(col)) {
                        *mapping = None;
                    }
                }
            }
            state.paste_dialog = Some(dialog)
        }
        Ok(_) => {}
        Err(error) => state.actions.push(GridAction::PasteFailed(error)),
    }
}

pub(super) fn mapping_ui(ui: &mut egui::Ui, state: &mut GridState, input: &GridInput<'_>) {
    let Some(mut dialog) = state.paste_dialog.take() else {
        return;
    };
    if let Some(work) = &dialog.work {
        match work.receiver.try_recv() {
            Ok(Ok(edits)) => {
                if state
                    .edits
                    .as_ref()
                    .is_some_and(|current| current.content_eq(&work.baseline))
                {
                    state.replace_edits(edits);
                    state.refocus = true;
                    return;
                }
                dialog.error = Some(
                    "The grid changed while preparing the paste. Review and try again.".into(),
                );
                dialog.work = None;
            }
            Ok(Err(error)) => {
                dialog.error = Some(error);
                dialog.work = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                dialog.error = Some("Could not prepare the pasted rows".into());
                dialog.work = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
    let working = dialog.work.is_some();
    let response = crate::theme::Dialog::new(
        ui.id().with("paste_rows_mapping"),
        crate::theme::icons::PASTE,
        "Paste as new rows",
    )
    .subtitle("Match source columns to destination columns")
    .width(620.0)
    .show(ui.ctx(), |d| {
        d.body(|ui| {
            if let Some(work) = &dialog.work {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!(
                        "Preparing {} of {} rows…",
                        work.processed.load(Ordering::Relaxed),
                        dialog.data.rows.len() - usize::from(dialog.header)
                    ));
                });
                return;
            }
            ui.label(format!(
                "{} rows · {} source columns",
                dialog.data.rows.len() - usize::from(dialog.header),
                dialog.data.columns.len()
            ));
            ui.label("Match source columns to destination columns. Unmapped columns are skipped.");
            ui.checkbox(&mut dialog.regenerate, "Regenerate automatic IDs");
            if dialog.external
                && ui
                    .checkbox(&mut dialog.header, "First row contains column names")
                    .changed()
            {
                dialog.mapping = dialog
                    .data
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        if !dialog.header {
                            return None;
                        }
                        let name = dialog.data.rows.first()?.get(i)?.as_ref()?.display();
                        input
                            .rows
                            .columns()
                            .iter()
                            .enumerate()
                            .find_map(|(col, c)| {
                                (c.name == name
                                    && state.edits.as_ref().is_some_and(|e| e.writable(col)))
                                .then_some(col)
                            })
                    })
                    .collect();
            }
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    egui::Grid::new("paste_column_mapping")
                        .striped(true)
                        .show(ui, |ui| {
                            ui.strong("Source");
                            ui.strong("Destination");
                            ui.end_row();
                            for (source, meta) in dialog.data.columns.iter().enumerate() {
                                let name = if dialog.header {
                                    dialog.data.rows[0]
                                        .get(source)
                                        .and_then(Option::as_ref)
                                        .map_or_else(
                                            || meta.name.clone(),
                                            |v| v.display().into_owned(),
                                        )
                                } else {
                                    meta.name.clone()
                                };
                                ui.label(name);
                                let label = dialog.mapping[source]
                                    .and_then(|c| input.rows.columns().get(c))
                                    .map_or("Skip", |c| c.name.as_str());
                                ComboBox::from_id_salt(("paste_target", source))
                                    .selected_text(label)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut dialog.mapping[source],
                                            None,
                                            "Skip",
                                        );
                                        for (col, target) in input.rows.columns().iter().enumerate()
                                        {
                                            if state.edits.as_ref().is_some_and(|e| e.writable(col))
                                            {
                                                let label = if dialog.regenerate
                                                    && state
                                                        .edits
                                                        .as_ref()
                                                        .is_some_and(|e| e.automatic(col))
                                                {
                                                    format!(
                                                        "{} (generated automatically)",
                                                        target.name
                                                    )
                                                } else {
                                                    target.name.clone()
                                                };
                                                ui.selectable_value(
                                                    &mut dialog.mapping[source],
                                                    Some(col),
                                                    label,
                                                );
                                            }
                                        }
                                    });
                                ui.end_row();
                            }
                        });
                });
            ui.separator();
            ui.label("Preview (first 5 rows)");
            egui::ScrollArea::horizontal().show(ui, |ui| {
                egui::Grid::new("paste_rows_preview")
                    .striped(true)
                    .show(ui, |ui| {
                        for values in dialog
                            .data
                            .rows
                            .iter()
                            .skip(usize::from(dialog.header))
                            .take(5)
                        {
                            for value in values {
                                ui.label(value.as_ref().map_or_else(
                                    || "(not selected)".into(),
                                    |v| v.display().into_owned(),
                                ));
                            }
                            ui.end_row();
                        }
                    });
            });
            if let Some(error) = &dialog.error {
                ui.colored_label(crate::theme::palette(ui.ctx()).error, error);
            }
        });
        d.footer(|f| {
            let apply = !working && f.primary("Add pending rows", None);
            f.cancel();
            apply
        })
    });
    let (apply, cancel) = (response.inner, response.closed);
    if apply && dialog.data.rows.len() > 1000 {
        if let Some(edits) = &state.edits {
            dialog.start(ui.ctx(), edits, input.rows);
        }
    } else if apply {
        match dialog.append(state, input) {
            Ok(_) => {
                state.refocus = true;
                return;
            }
            Err(error) => dialog.error = Some(error),
        }
    }
    if !cancel {
        state.paste_dialog = Some(dialog);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditBuffer;
    use sqlr_core::{ColumnOrigin, RowBuffer, TableColumn, TableInfo, TableRef};

    fn destination() -> (RowBuffer, EditBuffer) {
        let table = TableRef {
            database: None,
            schema: None,
            name: "target".into(),
        };
        let columns: Vec<_> = [
            ("id", ColumnKind::Int),
            ("name", ColumnKind::Text),
            ("email", ColumnKind::Text),
        ]
        .into_iter()
        .map(|(name, kind)| {
            ColumnMeta::new(name, "text", kind, DisplayHint::Plain).with_origin(Some(
                ColumnOrigin {
                    table: table.clone(),
                    column: name.into(),
                },
            ))
        })
        .collect();
        let info = TableInfo {
            table,
            primary_key: vec!["id".into()],
            unique_keys: vec![],
            columns: vec![
                TableColumn {
                    name: "id".into(),
                    type_name: "integer".into(),
                    nullable: false,
                    default: None,
                    generated: false,
                    automatic: true,
                },
                TableColumn {
                    name: "name".into(),
                    type_name: "text".into(),
                    nullable: false,
                    default: None,
                    generated: false,
                    automatic: false,
                },
                TableColumn {
                    name: "email".into(),
                    type_name: "text".into(),
                    nullable: true,
                    default: None,
                    generated: false,
                    automatic: false,
                },
            ],
        };
        let edits = EditBuffer::for_table(&columns, &info).unwrap();
        (RowBuffer::new(columns.into()), edits)
    }

    fn source(names: &[&str], values: Vec<Option<Value>>) -> Arc<GridClipboard> {
        Arc::new(GridClipboard {
            columns: names
                .iter()
                .map(|n| ColumnMeta::new(*n, "text", ColumnKind::Mixed, DisplayHint::Plain))
                .collect(),
            rows: vec![values],
            text: String::new(),
        })
    }

    #[test]
    fn mapping_uses_names_instead_of_source_order_and_regenerates_ids() {
        let (rows, edits) = destination();
        let input = GridInput {
            rows: &rows,
            more_available: false,
            dialect: None,
            table: None,
        };
        let data = source(
            &["email", "name", "id"],
            vec![
                Some(Value::Null),
                Some(Value::Text("Ada".into())),
                Some(Value::Int(42)),
            ],
        );
        let dialog = PasteRowsDialog::new(data, false, &input);
        let mut grid = GridState {
            edits: Some(edits),
            ..Default::default()
        };
        assert_eq!(dialog.append(&mut grid, &input), Ok(1));
        let edits = grid.edits.as_ref().unwrap();
        let row = edits.inserted_row_numbers()[0];
        assert_eq!(edits.value(&rows, row, 0), None);
        assert_eq!(edits.value(&rows, row, 1), Some(Value::Text("Ada".into())));
        assert_eq!(edits.value(&rows, row, 2), Some(Value::Null));
        grid.undo();
        assert_eq!(grid.edits.as_ref().unwrap().inserted_rows(), 0);
        grid.redo();
        assert_eq!(grid.edits.as_ref().unwrap().inserted_rows(), 1);
    }

    #[test]
    fn missing_required_column_and_duplicate_mapping_leave_no_pending_rows() {
        let (rows, edits) = destination();
        let input = GridInput {
            rows: &rows,
            more_available: false,
            dialect: None,
            table: None,
        };
        let data = source(
            &["first_name", "other_name"],
            vec![
                Some(Value::Text("Ada".into())),
                Some(Value::Text("Lovelace".into())),
            ],
        );
        let mut dialog = PasteRowsDialog::new(data, false, &input);
        let mut grid = GridState {
            edits: Some(edits),
            ..Default::default()
        };
        assert!(
            dialog
                .append(&mut grid, &input)
                .unwrap_err()
                .contains("required column name")
        );
        dialog.mapping = vec![Some(1), Some(1)];
        assert!(
            dialog
                .append(&mut grid, &input)
                .unwrap_err()
                .contains("more than once")
        );
        assert_eq!(grid.edits.as_ref().unwrap().pending(), 0);
        assert!(!grid.can_undo());
        dialog.mapping = vec![Some(1), Some(2)];
        assert_eq!(dialog.append(&mut grid, &input), Ok(1));
    }

    #[test]
    fn explicit_ids_and_empty_strings_are_preserved_when_requested() {
        let (rows, edits) = destination();
        let input = GridInput {
            rows: &rows,
            more_available: false,
            dialect: None,
            table: None,
        };
        let data = source(
            &["id", "name"],
            vec![Some(Value::Int(42)), Some(Value::Text(String::new()))],
        );
        let mut dialog = PasteRowsDialog::new(data, false, &input);
        dialog.regenerate = false;
        let mut grid = GridState {
            edits: Some(edits),
            ..Default::default()
        };
        assert_eq!(dialog.append(&mut grid, &input), Ok(1));
        let edits = grid.edits.as_ref().unwrap();
        let row = edits.inserted_row_numbers()[0];
        assert_eq!(edits.value(&rows, row, 0), Some(Value::Int(42)));
        assert_eq!(edits.value(&rows, row, 1), Some(Value::Text(String::new())));
        assert_eq!(edits.value(&rows, row, 2), None);
    }
}
