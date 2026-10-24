//! egui widgets for sqlr: the query editor, result grid, value panel, schema tree, connection
//! form, command palette, history panel and snippets panel.

mod admin;
mod chrome;
mod console;
mod datagen;
mod diff_tree;
mod doc_tree;
mod driver_manager;
pub mod editor;
mod er_canvas;
mod form;
mod grid;
mod history;
mod json_view;
mod key_browser;
mod key_value_view;
pub mod keymap;
pub mod keymap_editor;
mod mapping;
mod new_key;
mod objects;
pub mod operations_panel;
mod palette;
mod plan_view;
mod prefs_widgets;
mod result_bar;
mod snippets;
mod tasks_panel;
mod text;
mod text_menu;
pub mod theme;
mod tree;
mod value_panel;
mod welcome;
mod wizard;

pub use admin::{
    AdminPanel, PrivilegeCell, PrivilegeGridState, PrivilegeRow, PropertiesState, RoleEditorState,
    SessionsAction, SessionsState, StatsColumn, StatsState, blocking_tree, database_sizes_view,
    human_bytes, locks_view, privilege_grid, properties_view, role_form, role_list, sessions_table,
    table_stats_detail, table_stats_view,
};
pub use chrome::{
    ChipAction, ChipItem, ChipState, Crumb, ToastKind, Toasts, breadcrumb, connection_chip,
    empty_state, status_segment, tool_button,
};
pub use console::{ConsoleState, RespLine, RespLineKind, console, resp_lines};
pub use datagen::{datagen_table, generator_label};
pub use diff_tree::{DiffRow, DiffRowKind, diff_rows, diff_tree, text_diff};
pub use doc_tree::{DocAction, DocTreeState, doc_tree};
pub use driver_manager::{
    DriverJobView, DriverManagerAction, DriverManagerState, DriverRow, driver_manager,
};
pub use editor::{
    CARET_PLACEHOLDER, CHECKPOINT_BYTES, CONTEXT_CLIP_BYTES, Candidates, Caret, CaretContext,
    Change, CompletionItem, CompletionOptions, CompletionSource, ContextKind, EditCommand,
    EditorModel, EditorOutput, EditorState, FindOptions, FrameStats, INDENT, ItemKind, LEX_BUDGET,
    LONG_LINE_BYTES, MAX_ITEMS, Marker, Motion, NoCatalog, Pos, RelationRef, RunRequest,
    STRUCTURE_SYNC_LIMIT, ScopeWorker, StatementScope, SyntaxLayer, TextBuffer, UNDO_GROUP_GAP_S,
    UNDO_LIMIT, candidates, caret_context, query_editor, statement_scope,
};
pub use er_canvas::{ColumnsShown, ErAction, ErView, er_canvas, er_measure};
pub use form::{
    FormAction, FormState, TestStatus, connection_form, connection_form_body,
    connection_form_status, connection_identity,
};
pub use grid::{
    CopyFormat, DocTableState, EditBuffer, GridAction, GridInput, GridOutput, GridState,
    INSERTED_ROW_BASE, MAX_CELL_BYTES, MAX_CELL_CHARS, MAX_COPY_BYTES, MAX_COPY_CELLS,
    NEED_MORE_MARGIN, RowState, Selection, cell_display, change_tint, data_formats, data_grid,
    doc_table, is_inserted_row, parse_hex, parse_input, result_grid, set_data_formats,
    set_grid_clipboard,
};
pub use history::{HistoryAction, HistoryPanelState, format_duration, format_when, history_panel};
pub use json_view::{JsonViewState, json_view};
pub use key_browser::{
    KeyBrowserAction, KeyBrowserState, badge_color, badge_text, format_ttl, key_browser, type_badge,
};
pub use key_value_view::{KvViewAction, KvViewState, format_size, key_value_view, parse_ttl};
pub use mapping::mapping_table;
pub use new_key::{NewKeyDraft, NewKeyType, new_key_dialog};
pub use objects::{
    ObjectEditorAction, ObjectEditorContext, ObjectEditorState, ObjectSubTab, Problem, code_editor,
    highlighted, kind_icon, object_editor, sql_view, template_label, templates_for,
};
pub use palette::{
    PALETTE_ROWS, PaletteGroup, PaletteItem, PaletteMode, PaletteOutput, PaletteState, fuzzy_rank,
    palette,
};
pub use plan_view::{PlanAction, PlanChrome, PlanViewState, plan_view};
pub use prefs_widgets::{override_dot, setting_editor};
pub use result_bar::{
    ChangesAction, ResultBarAction, ResultBarView, read_only_bar, result_bar, result_bar_frame,
};
pub use snippets::{
    DraftResult, SnippetAction, SnippetDraft, SnippetsState, snippet_dialog, snippets_panel,
};
pub use tasks_panel::{RunningTask, TasksAction, tasks_panel};
pub use text::first_line;
pub use text_menu::{
    TextMenuMode, text_edit, text_edit_menu, text_edit_read_only, text_edit_secret,
};
pub use tree::{
    Children, ConnBadge, NodeMenu, SchemaSource, TreeAction, TreeConn, TreeDrop, TreeState,
    kind_label, schema_tree,
};
pub use value_panel::{ValueEdit, ValueEditAction, ValueEditMode, value_editor, value_panel};
pub use welcome::{DriverCard, RecentItem, WelcomeAction, welcome};
pub use wizard::{WizardAction, WizardStep, wizard_frame};
