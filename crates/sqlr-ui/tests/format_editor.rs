use sqlr_core::{FormatOptions, SqlSyntax};
use sqlr_ui::{Caret, EditorState, Pos};
#[test]
fn format_action_is_one_undo_group() {
    let mut editor = EditorState::new("select a,b from t", SqlSyntax::ansi());
    editor.format_sql(&FormatOptions::default());
    assert_eq!(editor.text(), "SELECT a, b\nFROM t");
    editor.apply_edit(sqlr_ui::EditCommand::Undo);
    assert_eq!(editor.text(), "select a,b from t");
    // Only the selected range is formatted.
    editor.set_carets(vec![Caret {
        anchor: Pos { line: 0, col: 0 },
        head: Pos { line: 0, col: 6 },
    }]);
    editor.format_sql(&FormatOptions::default());
    assert_eq!(editor.text(), "SELECT a,b from t");
}
#[test]
fn tab_and_newline_follow_indentation() {
    let mut editor = EditorState::new("(", SqlSyntax::ansi());
    editor.set_format_options(FormatOptions {
        indent: sqlr_core::Indent::Spaces(2),
        ..Default::default()
    });
    editor.set_carets(vec![Caret::at(Pos { line: 0, col: 1 })]);
    editor.apply_edit(sqlr_ui::EditCommand::Newline);
    assert_eq!(editor.text(), "(\n  ");
    editor.apply_edit(sqlr_ui::EditCommand::Tab);
    assert_eq!(editor.text(), "(\n    ");
}

#[test]
fn duplicate_line_is_undoable() {
    let mut editor = EditorState::new("select 1", SqlSyntax::ansi());
    editor.apply_edit(sqlr_ui::EditCommand::DuplicateLine);
    assert_eq!(editor.text(), "select 1\nselect 1");
    editor.apply_edit(sqlr_ui::EditCommand::Undo);
    assert_eq!(editor.text(), "select 1");
}
