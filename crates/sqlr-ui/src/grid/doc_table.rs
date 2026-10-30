//! The Table view of document results: the result grid over the documents flattened by the
//! Table rule (columns are the union of the first page's top-level keys, nested values
//! compact JSON). Later pages append rows under the same columns; the rows are built once
//! per page, not per frame.

use sqlr_core::{DocChunk, DocValue, RowBuffer, doc_column_metas, doc_columns, doc_rows_chunk};

use super::{GridAction, GridInput, GridState, data_grid};
use crate::doc_tree::DocAction;

/// The most memory the table's copy of the documents takes by default: past it, the table
/// stops (the Tree and JSON views still show every document).
pub const DOC_TABLE_BUDGET: usize = 128 << 20;

#[derive(Default)]
pub struct DocTableState {
    pub grid: GridState,
    rows: RowBuffer,
    /// Bytes the rows may take; 0 for [`DOC_TABLE_BUDGET`].
    budget: usize,
    /// Documents were left out at the budget.
    truncated: bool,
    /// Chunks already turned into rows.
    chunks_seen: usize,
    /// The document reported as selected last.
    reported: Option<usize>,
}

impl DocTableState {
    /// A table whose copy of the documents stops at `bytes`.
    pub fn with_budget(bytes: usize) -> Self {
        Self {
            budget: bytes,
            ..Self::default()
        }
    }

    /// Documents were left out of the table at its memory budget.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// The rows of `chunks` (columns named by the first `first_page` documents), updated
    /// for chunks added since the last call.
    pub fn rows(&mut self, chunks: &[DocChunk], first_page: usize) -> &RowBuffer {
        let first_documents = chunks.len() > self.chunks_seen
            && self.rows.columns().is_empty()
            && chunks[self.chunks_seen..].iter().any(|c| !c.is_empty());
        if chunks.len() < self.chunks_seen
            || (self.chunks_seen == 0 && !chunks.is_empty())
            || first_documents
        {
            let first: Vec<DocValue> = chunks
                .iter()
                .flat_map(DocChunk::docs)
                .take(first_page.max(1))
                .collect();
            let metas = doc_column_metas(&doc_columns(&first));
            self.rows = RowBuffer::new(metas);
            self.chunks_seen = 0;
            self.truncated = false;
        }
        let budget = if self.budget == 0 {
            DOC_TABLE_BUDGET
        } else {
            self.budget
        };
        let metas = self.rows.columns().to_vec().into();
        for chunk in &chunks[self.chunks_seen..] {
            // The first page always shows; later ones only while the copy is in budget.
            if self.rows.row_count() > 0 && self.rows.byte_size() >= budget {
                self.truncated = true;
                break;
            }
            if !chunk.is_empty() && !self.rows.columns().is_empty() {
                let docs: Vec<DocValue> = chunk.docs().collect();
                self.rows.push_chunk(doc_rows_chunk(&metas, &docs));
            }
        }
        self.chunks_seen = chunks.len();
        &self.rows
    }
}

/// Draws `chunks` as a table. A row selected reports its document; reaching the end with
/// `more` asks for the next page; a cell opened (double-click, Enter) edits its document.
pub fn doc_table(
    ui: &mut egui::Ui,
    state: &mut DocTableState,
    chunks: &[DocChunk],
    first_page: usize,
    more: bool,
) -> Option<DocAction> {
    state.rows(chunks, first_page);
    if state.truncated {
        ui.label(format!(
            "The table shows the first {} documents (its memory budget); Tree and JSON show \
             them all",
            state.rows.row_count()
        ));
    }
    let output = data_grid(
        ui,
        &mut state.grid,
        &GridInput {
            rows: &state.rows,
            more_available: more,
            dialect: None,
            table: None,
        },
    );
    let mut action = None;
    for a in state.grid.take_actions() {
        if let GridAction::EditCell { row, .. } | GridAction::OpenValue { row, .. } = a
            && row < state.rows.row_count()
        {
            action = Some(DocAction::Edit(row));
        }
    }
    let selected = state
        .grid
        .selected
        .map(|(row, _)| row)
        .filter(|r| *r < state.rows.row_count());
    let mut select = None;
    if selected != state.reported {
        state.reported = selected;
        select = selected.map(DocAction::Select);
    }
    action
        .or(select)
        .or(output.need_more.then_some(DocAction::LoadMore))
}

#[cfg(test)]
mod tests {
    use sqlr_core::{DocChunk, DocValue};

    use super::*;

    fn doc(value: i32) -> DocValue {
        DocValue::Document(vec![("n".into(), DocValue::Int32(value))])
    }

    #[test]
    fn documents_after_an_empty_first_page_are_shown() {
        let empty = DocChunk::default();
        let mut state = DocTableState::default();
        assert_eq!(state.rows(std::slice::from_ref(&empty), 1).row_count(), 0);

        // The next page has documents: the table must pick up its columns and rows.
        let chunks = vec![empty, DocChunk::from_docs(&[doc(1)])];
        let rows = state.rows(&chunks, 1);
        assert_eq!(rows.columns().len(), 1);
        assert_eq!(rows.row_count(), 1);
    }
}
