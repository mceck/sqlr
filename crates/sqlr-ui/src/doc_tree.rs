//! The Tree view of document results: one row per document, nested objects and arrays
//! expanded one level at a time. Only expanded documents are decoded (and kept); the rows
//! are virtualised, and rebuilt only when the documents or the expansion change.

use std::collections::{HashMap, HashSet};

use egui::{RichText, Sense, WidgetInfo, WidgetType, vec2};
use sqlr_core::{DocChunk, DocValue};

use crate::theme::{self, Spacing, icons};

const INDENT: f32 = 16.0;
const ARROW_WIDTH: f32 = 16.0;
/// Chars of a value shown on its row.
const VALUE_CHARS: usize = 120;

/// What the document views ask of their caller.
#[derive(Clone, Debug, PartialEq)]
pub enum DocAction {
    /// The document became the selected one (the JSON view and the value panel follow).
    Select(usize),
    /// Open the document in the JSON view, editable.
    Edit(usize),
    /// Save the document's new content (replaced by its `_id`).
    Save(usize, DocValue),
    /// Insert a new document.
    Insert(DocValue),
    /// Delete the documents (after a confirmation).
    Delete(Vec<usize>),
    /// Copy the document as JSON.
    CopyJson(usize),
    /// Fetch the next page.
    LoadMore,
}

/// A step into a nested value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Seg {
    Key(usize, String),
    Index(usize),
}

#[derive(Clone)]
struct Row {
    doc: usize,
    path: Vec<Seg>,
}

#[derive(Default)]
pub struct DocTreeState {
    /// The selected document.
    pub selected: Option<usize>,
    field: Option<(usize, Vec<Seg>)>,
    expanded: HashSet<(usize, Vec<Seg>)>,
    /// Expanded documents, decoded.
    decoded: HashMap<usize, DocValue>,
    /// One line per document row seen (its `_id` and field count), and whether it has an
    /// `_id` (only then can it be edited or deleted).
    summaries: HashMap<usize, (String, bool)>,
    rows: Vec<Row>,
    /// Documents the rows were built for; `None` forces a rebuild.
    built_for: Option<usize>,
}

/// The `i`-th document of `chunks`.
fn doc_at(chunks: &[DocChunk], mut i: usize) -> Option<DocValue> {
    for chunk in chunks {
        if i < chunk.len() {
            return Some(chunk.doc(i));
        }
        i -= chunk.len();
    }
    None
}

fn child<'a>(v: &'a DocValue, seg: &Seg) -> Option<&'a DocValue> {
    match (v, seg) {
        (DocValue::Document(fields), Seg::Key(i, _)) => fields.get(*i).map(|(_, v)| v),
        (DocValue::Array(items), Seg::Index(i)) => items.get(*i),
        _ => None,
    }
}

fn at<'a>(doc: &'a DocValue, path: &[Seg]) -> Option<&'a DocValue> {
    path.iter().try_fold(doc, |v, seg| child(v, seg))
}

fn children(v: &DocValue) -> Vec<Seg> {
    match v {
        DocValue::Document(fields) => fields
            .iter()
            .enumerate()
            .map(|(i, (k, _))| Seg::Key(i, k.clone()))
            .collect(),
        DocValue::Array(items) => (0..items.len()).map(Seg::Index).collect(),
        _ => Vec::new(),
    }
}

fn seg_label(seg: &Seg) -> String {
    match seg {
        Seg::Key(_, k) => k.clone(),
        Seg::Index(i) => format!("[{i}]"),
    }
}

/// `a.b[2].c`.
fn path_text(path: &[Seg]) -> String {
    let mut out = String::new();
    for seg in path {
        match seg {
            Seg::Key(_, k) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(k);
            }
            Seg::Index(i) => out.push_str(&format!("[{i}]")),
        }
    }
    out
}

/// A value on one line: scalars as relaxed JSON (cut), containers by size.
pub(crate) fn value_line(v: &DocValue) -> String {
    match v {
        DocValue::Document(f) => format!("{{ {} fields }}", f.len()),
        DocValue::Array(items) => format!("[ {} items ]", items.len()),
        other => {
            let text = other.to_relaxed_json();
            match text.char_indices().nth(VALUE_CHARS) {
                Some((cut, _)) => format!("{}…", &text[..cut]),
                None => text,
            }
        }
    }
}

fn summary(doc: &DocValue) -> String {
    let n = match doc {
        DocValue::Document(f) => f.len(),
        _ => 0,
    };
    match doc.get("_id") {
        Some(id) => format!("_id: {} · {n} fields", value_line(id)),
        None => format!("{n} fields"),
    }
}

impl DocTreeState {
    /// Documents decoded for display (only the expanded ones).
    pub fn decoded(&self) -> usize {
        self.decoded.len()
    }

    /// The selected field: its dotted path and value.
    pub fn selected_field(&self) -> Option<(String, DocValue)> {
        let (doc, path) = self.field.as_ref()?;
        let value = at(self.decoded.get(doc)?, path)?.clone();
        Some((path_text(path), value))
    }

    /// Forgets everything shown (a new result).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn toggle(&mut self, chunks: &[DocChunk], doc: usize, path: Vec<Seg>) {
        let key = (doc, path);
        if !self.expanded.remove(&key) {
            if key.1.is_empty()
                && !self.decoded.contains_key(&doc)
                && let Some(d) = doc_at(chunks, doc)
            {
                self.decoded.insert(doc, d);
            }
            self.expanded.insert(key);
        } else if key.1.is_empty() {
            // A collapsed document is decoded again when it is next expanded.
            self.expanded.retain(|(d, _)| *d != doc);
            self.decoded.remove(&doc);
            if self.field.as_ref().is_some_and(|(d, _)| *d == doc) {
                self.field = None;
            }
        }
        self.built_for = None;
    }

    fn rebuild(&mut self, total: usize) {
        if self.built_for == Some(total) {
            return;
        }
        let mut rows = Vec::with_capacity(total);
        for doc in 0..total {
            rows.push(Row {
                doc,
                path: Vec::new(),
            });
            if let Some(value) = self
                .decoded
                .get(&doc)
                .filter(|_| self.expanded.contains(&(doc, Vec::new())))
            {
                self.push_children(doc, value, Vec::new(), &mut rows);
            }
        }
        self.rows = rows;
        self.built_for = Some(total);
    }

    fn push_children(&self, doc: usize, value: &DocValue, path: Vec<Seg>, rows: &mut Vec<Row>) {
        let Some(v) = at(value, &path) else {
            return;
        };
        for seg in children(v) {
            let mut p = path.clone();
            p.push(seg);
            rows.push(Row {
                doc,
                path: p.clone(),
            });
            if self.expanded.contains(&(doc, p.clone())) {
                self.push_children(doc, value, p, rows);
            }
        }
    }
}

/// Draws the documents of `chunks` as a tree. With `editable`, each document's menu offers
/// edit, duplicate and delete.
pub fn doc_tree(
    ui: &mut egui::Ui,
    state: &mut DocTreeState,
    chunks: &[DocChunk],
    editable: bool,
) -> Option<DocAction> {
    let total: usize = chunks.iter().map(DocChunk::len).sum();
    state.rebuild(total);
    let p = theme::palette(ui.ctx());
    let mut action = None;
    let mut toggle: Option<(usize, Vec<Seg>)> = None;
    let row_height = Spacing::ROW_TREE;
    egui::ScrollArea::both()
        .id_salt("doc_tree")
        .auto_shrink(false)
        .show_rows(ui, row_height, state.rows.len(), |ui, range| {
            for r in range {
                let Row { doc, path } = state.rows[r].clone();
                ui.horizontal(|ui| {
                    ui.set_min_height(row_height);
                    ui.add_space(path.len() as f32 * INDENT);
                    let value = state.decoded.get(&doc).and_then(|d| at(d, &path));
                    let container = path.is_empty()
                        || matches!(value, Some(DocValue::Document(_) | DocValue::Array(_)));
                    let name = if path.is_empty() {
                        format!("document {}", doc + 1)
                    } else {
                        seg_label(path.last().expect("a field row has a path"))
                    };
                    if container {
                        let open = state.expanded.contains(&(doc, path.clone()));
                        let (rect, arrow) =
                            ui.allocate_exact_size(vec2(ARROW_WIDTH, row_height), Sense::click());
                        let verb = if open { "Collapse" } else { "Expand" };
                        arrow.widget_info(|| {
                            WidgetInfo::labeled(WidgetType::Button, true, format!("{verb} {name}"))
                        });
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            if open {
                                icons::CARET_DOWN
                            } else {
                                icons::CARET_RIGHT
                            },
                            icons::font(ui, 12.0),
                            p.text_muted,
                        );
                        if arrow.clicked() {
                            toggle = Some((doc, path.clone()));
                        }
                    } else {
                        ui.add_space(ARROW_WIDTH);
                    }
                    if path.is_empty() {
                        let (line, has_id) = state
                            .summaries
                            .entry(doc)
                            .or_insert_with(|| {
                                doc_at(chunks, doc).map_or_else(
                                    || (String::new(), false),
                                    |d| (summary(&d), d.get("_id").is_some()),
                                )
                            })
                            .clone();
                        // A document without `_id` cannot be found again: read-only.
                        let editable = editable && has_id;
                        let selected = state.selected == Some(doc);
                        let label = ui.selectable_label(selected, format!("({}) {line}", doc + 1));
                        if label.clicked() {
                            state.selected = Some(doc);
                            state.field = None;
                            action = Some(DocAction::Select(doc));
                        }
                        label.context_menu(|ui| {
                            if editable && ui.button("Edit document").clicked() {
                                action = Some(DocAction::Edit(doc));
                                ui.close();
                            }
                            if ui.button("Copy JSON").clicked() {
                                action = Some(DocAction::CopyJson(doc));
                                ui.close();
                            }
                            if editable && ui.button("Duplicate").clicked() {
                                if let Some(DocValue::Document(fields)) = doc_at(chunks, doc) {
                                    let copy =
                                        fields.into_iter().filter(|(k, _)| k != "_id").collect();
                                    action = Some(DocAction::Insert(DocValue::Document(copy)));
                                }
                                ui.close();
                            }
                            if editable && ui.button("Delete document…").clicked() {
                                action = Some(DocAction::Delete(vec![doc]));
                                ui.close();
                            }
                        });
                    } else {
                        let selected = state.field.as_ref() == Some(&(doc, path.clone()));
                        if ui.selectable_label(selected, &name).clicked() {
                            state.field = Some((doc, path.clone()));
                            state.selected = Some(doc);
                            action = Some(DocAction::Select(doc));
                        }
                        if let Some(v) = value {
                            ui.label(RichText::new(value_line(v)).monospace());
                            ui.label(RichText::new(v.type_name()).small().color(p.text_muted));
                        }
                    }
                });
            }
        });
    if let Some((doc, path)) = toggle {
        state.toggle(chunks, doc, path);
    }
    action
}
