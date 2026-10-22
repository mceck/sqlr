//! The SQL query editor's text layers: a rope buffer, the multi-caret edit model with grouped
//! undo, plain-text find, the resumable line-state syntax layer, the statement-scoped
//! completion scope and the completion engine, which hold no egui state; and the `QueryEditor`
//! widget over them.

mod buffer;
mod complete;
mod find;
mod find_bar;
mod input;
mod layout;
mod model;
mod popup;
mod scope;
mod syntax;
mod undo;
mod widget;
mod wrap;

pub use buffer::{Change, Pos, TextBuffer};
pub use complete::{
    CONTEXT_CLIP_BYTES, Candidates, CaretContext, CompletionItem, CompletionSource, ContextKind,
    ItemKind, MAX_ITEMS, NoCatalog, candidates, caret_context,
};
pub use find::FindOptions;
pub use model::{Caret, EditCommand, EditorModel, INDENT, Motion};
pub use scope::{
    CARET_PLACEHOLDER, RelationRef, STRUCTURE_SYNC_LIMIT, ScopeWorker, StatementScope,
    statement_scope,
};
pub use syntax::{CHECKPOINT_BYTES, LEX_BUDGET, LONG_LINE_BYTES, SyntaxLayer};
pub use undo::{UNDO_GROUP_GAP_S, UNDO_LIMIT};
pub use widget::{
    CompletionOptions, EditorOutput, EditorState, FrameStats, Marker, RunRequest, query_editor,
};
