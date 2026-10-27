//! The JSON view of one document: its relaxed Extended JSON, read-only; "Edit" turns it
//! into an editor (Mongo shell literals such as `ObjectId("…")` are accepted), and "Save"
//! validates the text before it reports the new document. A document holding a type sqlr
//! cannot write back stays read-only, and the view says why.

use egui::RichText;
use sqlr_core::{DocValue, QueryLanguage, SqlSyntax};

use crate::doc_tree::DocAction;
use crate::editor::{EditorState, NoCatalog, query_editor};
use crate::theme::{self, Spacing};

/// Why the text of a document editor cannot be saved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentTextError {
    /// The text is not valid relaxed Extended JSON.
    #[error("{message} (line {line}, column {column})")]
    Syntax {
        /// The parser's complaint.
        message: String,
        /// The 1-based line.
        line: usize,
        /// The 1-based column.
        column: usize,
    },
    /// The text is valid JSON but not an object.
    #[error("A document is an object, not {type_name}")]
    NotAnObject {
        /// The type of the value found.
        type_name: &'static str,
    },
    /// The document holds a type sqlr cannot write.
    #[error("The document holds {what}, which sqlr cannot write")]
    Unwritable {
        /// The type found.
        what: &'static str,
    },
}

/// What the editor holds.
struct Edit {
    /// The document edited; `None` for a new one.
    index: Option<usize>,
    editor: Box<EditorState>,
}

#[derive(Default)]
pub struct JsonViewState {
    edit: Option<Edit>,
    error: Option<String>,
    /// Why "Edit" was refused, for the document it was refused for.
    refused: Option<(usize, String)>,
    /// The shown document's text, by index and the document it was built from.
    shown: Option<(usize, DocValue, String)>,
    /// Save the editor when the view next draws, as its Save button would.
    save_requested: bool,
}

fn editor_for(text: &str) -> Box<EditorState> {
    let mut editor = EditorState::new(text, SqlSyntax::ansi());
    editor.set_language(QueryLanguage::MongoShell);
    // A document has no names to complete.
    editor.set_completion(false);
    Box::new(editor)
}

/// `position` (a byte offset of `text`) as a 1-based line and column.
fn line_col(text: &str, position: usize) -> (usize, usize) {
    let before = &text[..text.floor_char_boundary(position.min(text.len()))];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

impl JsonViewState {
    /// The editor is open.
    pub fn editing(&self) -> bool {
        self.edit.is_some()
    }

    /// The editor's text, while it is open.
    pub fn edit_text(&self) -> Option<String> {
        self.edit.as_ref().map(|e| e.editor.text())
    }

    /// Replaces the editor's text (while it is open).
    pub fn set_edit_text(&mut self, text: &str) {
        if let Some(edit) = &mut self.edit {
            edit.editor = editor_for(text);
            self.error = None;
        }
    }

    /// Why the last save was refused.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Opens the editor on `doc` (the document at `index`); refused for a document holding
    /// a type sqlr cannot write back.
    pub fn begin_edit(&mut self, index: usize, doc: &DocValue) {
        if let Some(what) = doc.unmodelled() {
            self.refused = Some((
                index,
                format!(
                    "This document holds {what}, which sqlr cannot write back: it is read-only"
                ),
            ));
            return;
        }
        self.refused = None;
        self.error = None;
        self.edit = Some(Edit {
            index: Some(index),
            editor: editor_for(&doc.to_relaxed_json_pretty()),
        });
    }

    /// Opens the editor on a new document, starting from `template`.
    pub fn begin_insert(&mut self, template: &DocValue) {
        self.refused = None;
        self.error = None;
        self.edit = Some(Edit {
            index: None,
            editor: editor_for(&template.to_relaxed_json_pretty()),
        });
    }

    /// Saves the open editor when the view next draws, as its Save button would: the text
    /// is checked first, and an invalid one keeps the editor open with its message.
    pub fn request_save(&mut self) {
        self.save_requested = self.edit.is_some();
    }

    /// Closes the editor without saving.
    pub fn cancel(&mut self) {
        self.edit = None;
        self.error = None;
    }

    /// Checks the editor's text: the document, or why it cannot be saved.
    fn validate(text: &str) -> Result<DocValue, DocumentTextError> {
        let doc = DocValue::from_relaxed_json(text).map_err(|e| {
            let (line, column) = line_col(text, e.position);
            DocumentTextError::Syntax {
                message: e.message,
                line,
                column,
            }
        })?;
        if !matches!(doc, DocValue::Document(_)) {
            return Err(DocumentTextError::NotAnObject {
                type_name: doc.type_name(),
            });
        }
        if let Some(what) = doc.unmodelled() {
            return Err(DocumentTextError::Unwritable { what });
        }
        Ok(doc)
    }
}

/// Draws the document at `index`; with `editable`, it can be edited and saved.
pub fn json_view(
    ui: &mut egui::Ui,
    state: &mut JsonViewState,
    index: usize,
    doc: &DocValue,
    editable: bool,
) -> Option<DocAction> {
    let p = theme::palette(ui.ctx());
    let mut action = None;
    let requested = std::mem::take(&mut state.save_requested);
    if let Some(edit) = &mut state.edit {
        let mut save = requested;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let what = match edit.index {
                Some(i) => format!("Editing document {}", i + 1),
                None => "New document".to_string(),
            };
            ui.label(RichText::new(what).strong());
            save |= ui.button("Save").clicked();
            cancel = ui.button("Cancel").clicked();
        });
        if let Some(error) = &state.error {
            ui.colored_label(p.error, error);
        }
        ui.add_space(Spacing::S);
        query_editor(ui, &mut edit.editor, &NoCatalog);
        if cancel {
            state.cancel();
        } else if save {
            match JsonViewState::validate(&edit.editor.text()) {
                Ok(doc) => {
                    action = Some(match edit.index {
                        Some(i) => DocAction::Save(i, doc),
                        None => DocAction::Insert(doc),
                    });
                    // The caller closes the editor once the save worked (`cancel`); a
                    // failed save keeps the text.
                    state.error = None;
                }
                Err(e) => state.error = Some(e.to_string()),
            }
        }
        return action;
    }
    ui.horizontal(|ui| {
        if editable && ui.button("Edit document").clicked() {
            state.begin_edit(index, doc);
        }
        if ui.button("Copy JSON").clicked() {
            action = Some(DocAction::CopyJson(index));
        }
    });
    if let Some((i, reason)) = &state.refused
        && *i == index
    {
        ui.colored_label(p.warning, reason);
    }
    // The text is rebuilt only when another document is shown.
    let stale = !matches!(&state.shown, Some((i, d, _)) if *i == index && d == doc);
    if stale {
        state.shown = Some((index, doc.clone(), doc.to_relaxed_json_pretty()));
    }
    if let Some((_, _, text)) = &state.shown {
        egui::ScrollArea::both()
            .id_salt("json_view")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(text).monospace()).selectable(true));
            });
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_name_the_line() {
        assert_eq!(line_col("ab\ncd", 4), (2, 2));
        let e = JsonViewState::validate("{\"a\": 1,\n \"b\": }")
            .unwrap_err()
            .to_string();
        assert!(e.contains("line 2"), "{e}");
        assert_eq!(
            JsonViewState::validate("[1]").unwrap_err().to_string(),
            "A document is an object, not array"
        );
    }
}
