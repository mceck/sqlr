//! Whole-operation history and atomic edits over multiple selections.

use super::{EditBuffer, GridAction, GridState};
use sqlr_core::{RowBuffer, Value};

#[derive(Default)]
pub(super) struct EditHistory {
    undo: Vec<EditBuffer>,
    redo: Vec<EditBuffer>,
}

impl EditHistory {
    fn push(stack: &mut Vec<EditBuffer>, edits: EditBuffer) {
        stack.push(edits);
        let mut bytes: usize = stack.iter().map(EditBuffer::history_bytes).sum();
        while stack.len() > 1 && (stack.len() > 100 || bytes > 32 * 1024 * 1024) {
            bytes = bytes.saturating_sub(stack.remove(0).history_bytes());
        }
    }
}

impl GridState {
    pub fn can_undo(&self) -> bool {
        self.edits.is_some() && !self.history.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        self.edits.is_some() && !self.history.redo.is_empty()
    }

    pub fn undo(&mut self) {
        if !self.can_undo() {
            return;
        }
        if let Some(previous) = self.history.undo.pop() {
            let current = self.edits.replace(previous).expect("editable grid");
            EditHistory::push(&mut self.history.redo, current);
            self.actions.push(GridAction::EditsChanged);
        }
    }

    pub fn redo(&mut self) {
        if !self.can_redo() {
            return;
        }
        if let Some(next) = self.history.redo.pop() {
            let current = self.edits.replace(next).expect("editable grid");
            EditHistory::push(&mut self.history.undo, current);
            self.actions.push(GridAction::EditsChanged);
        }
    }

    pub fn discard_edits(&mut self) {
        if let Some(edits) = &mut self.edits {
            edits.clear();
        }
        self.history = EditHistory::default();
    }

    pub(super) fn record_edits(&mut self, before: EditBuffer) {
        if self.edits.as_ref().is_some_and(|e| !e.content_eq(&before)) {
            EditHistory::push(&mut self.history.undo, before);
            self.history.redo.clear();
        }
    }

    pub(super) fn replace_edits(&mut self, edits: EditBuffer) {
        let previous_rows = self.edits.as_ref().map_or(0, EditBuffer::inserted_rows);
        if edits.inserted_rows() > previous_rows {
            self.reveal = edits.inserted_number_at(previous_rows);
        }
        if let Some(before) = self.edits.replace(edits) {
            self.record_edits(before);
        }
        self.actions.push(GridAction::EditsChanged);
    }

    pub(super) fn set_selected_null(&mut self, rows: &RowBuffer) {
        let Some(mut trial) = self.edits.clone() else {
            return;
        };
        let mut selected = Vec::new();
        for view in self.selected_view_rows() {
            let Some(source) = self.source(view) else {
                continue;
            };
            for col in self.selected_cols() {
                if self.is_selected(view, col) {
                    if let Err(error) = trial.set(rows, source, col, Value::Null) {
                        self.actions
                            .push(GridAction::PasteFailed(format!("Cannot set NULL: {error}")));
                        return;
                    }
                    selected.push((source, col));
                }
            }
        }

        self.replace_edits(trial);
        self.actions
            .extend(selected.into_iter().map(|(row, col)| GridAction::SetCell {
                row,
                col,
                value: Value::Null,
            }));
    }

    pub(super) fn revert_selected_cells(&mut self) {
        let Some(mut trial) = self.edits.clone() else {
            return;
        };
        for view in self.selected_view_rows() {
            let Some(source) = self.source(view) else {
                continue;
            };
            for col in self.selected_cols() {
                if self.is_selected(view, col) {
                    trial.revert_cell(source, col);
                }
            }
        }
        self.replace_edits(trial);
    }
}
