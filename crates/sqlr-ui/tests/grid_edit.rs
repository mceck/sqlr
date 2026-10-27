//! The result grid's selection, sorting, filtering, copying and editing.

use std::sync::Arc;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sqlr_core::{Cell, ChunkBuilder, ColumnKind, ColumnMeta, DisplayHint, RowBuffer, Value};
use sqlr_ui::{
    ChangesAction, EditBuffer, GridAction, GridInput, GridState, ResultBarAction, ResultBarView,
    data_grid, read_only_bar, result_bar,
};

fn columns() -> Arc<[ColumnMeta]> {
    Arc::from(vec![
        ColumnMeta::new("id", "int8", ColumnKind::Int, DisplayHint::Number),
        ColumnMeta::new("name", "text", ColumnKind::Text, DisplayHint::Plain),
    ])
}

/// `ids[i]` (NULL for `None`) and "name i".
fn buffer(ids: &[Option<i64>]) -> RowBuffer {
    let cols = columns();
    let mut buf = RowBuffer::new(cols.clone());
    let mut b = ChunkBuilder::new(cols);
    for (i, id) in ids.iter().enumerate() {
        let name = format!("name {i}");
        b.push_row(&[id.map_or(Cell::Null, Cell::Int), Cell::Text(&name)])
            .unwrap();
    }
    buf.push_chunk(b.finish());
    buf
}

fn sequential(n: usize) -> RowBuffer {
    buffer(&(0..n as i64).map(Some).collect::<Vec<_>>())
}

struct App {
    state: GridState,
    rows: RowBuffer,
    more: bool,
    actions: Vec<GridAction>,
}

fn harness_with(rows: RowBuffer, more: bool, edits: Option<EditBuffer>) -> Harness<'static, App> {
    let mut state = GridState::default();
    state.edits = edits;
    let app = App {
        state,
        rows,
        more,
        actions: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(1000.0, 600.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                let input = GridInput {
                    rows: &app.rows,
                    more_available: app.more,
                    dialect: None,
                    table: Some("orders"),
                };
                data_grid(ui, &mut app.state, &input);
                app.actions.extend(app.state.take_actions());
            },
            app,
        );
    h.run_steps(2);
    h
}

fn harness(rows: RowBuffer) -> Harness<'static, App> {
    harness_with(rows, false, None)
}

/// An edit buffer for `id` (the key, NOT NULL) and `name` (nullable), both writable.
fn editable() -> EditBuffer {
    EditBuffer::new(vec![0], vec![true, true], vec![true, false])
}

fn center(h: &Harness<'static, App>, label: &str) -> Pos2 {
    h.get_by_label(label).rect().center()
}

fn press_at(h: &mut Harness<'static, App>, pos: Pos2, button: PointerButton, modifiers: Modifiers) {
    h.event(Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        });
    }
    h.run_steps(2);
}

fn click(h: &mut Harness<'static, App>, label: &str) {
    let pos = center(h, label);
    press_at(h, pos, PointerButton::Primary, Modifiers::NONE);
}

fn double_click(h: &mut Harness<'static, App>, label: &str) {
    h.run_steps(100);
    let pos = center(h, label);
    h.event(Event::PointerMoved(pos));
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        h.step();
    }
    h.run_steps(2);
}

/// Opens the context menu on `label` and clicks `item`.
fn menu(h: &mut Harness<'static, App>, label: &str, item: &str) {
    h.run_steps(100);
    let pos = center(h, label);
    press_at(h, pos, PointerButton::Secondary, Modifiers::NONE);
    h.get_by_label(item).click();
    h.run_steps(2);
}

fn copied(h: &Harness<'static, App>) -> Option<String> {
    h.output()
        .platform_output
        .commands
        .iter()
        .find_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        })
}

fn press(h: &mut Harness<'static, App>, modifiers: Modifiers, key: Key) {
    h.key_press_modifiers(modifiers, key);
    h.run_steps(2);
}

#[test]
fn row_numbers_shown() {
    let h = harness(sequential(30));
    h.get_by_label("Row 1");
    h.get_by_label("Row 2");
    h.get_by_label("Row 20");
    // Row numbers are not data: no data cell holds them.
    assert!(h.query_by_label("Row 0").is_none());
}

#[test]
fn row_number_click_selects_the_row() {
    let mut h = harness(sequential(30));
    click(&mut h, "Row 2");
    let s = h.state().state.selection;
    assert_eq!((s.anchor, s.cursor), ((1, 0), (1, 1)));
    h.event(Event::Copy);
    h.step();
    assert_eq!(copied(&h).as_deref(), Some("1\tname 1\n"));
}

#[test]
fn shift_arrow_extends_selection_and_copy_tsv() {
    let mut h = harness(sequential(30));
    click(&mut h, "0");
    press(&mut h, Modifiers::SHIFT, Key::ArrowDown);
    press(&mut h, Modifiers::SHIFT, Key::ArrowRight);
    let s = h.state().state.selection;
    assert_eq!((s.anchor, s.cursor), ((0, 0), (1, 1)));
    h.event(Event::Copy);
    h.step();
    assert_eq!(copied(&h).as_deref(), Some("0\tname 0\n1\tname 1\n"));

    // ⌘A selects everything; whole columns are copied with their names.
    press(&mut h, Modifiers::COMMAND, Key::A);
    h.event(Event::Copy);
    h.step();
    let text = copied(&h).unwrap();
    assert!(text.starts_with("id\tname\n0\tname 0\n"), "{text}");
    assert_eq!(text.lines().count(), 31);
}

#[test]
fn command_click_builds_a_non_contiguous_selection() {
    let mut h = harness(sequential(5));
    click(&mut h, "name 0");
    h.get_by_label("name 2").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    assert!(h.state().state.is_selected(0, 1));
    assert!(h.state().state.is_selected(2, 1));
    assert!(!h.state().state.is_selected(1, 1));
    h.event(Event::Copy);
    h.step();
    assert_eq!(copied(&h).as_deref(), Some("name 0\nname 2\n"));
}

#[test]
fn command_click_a_header_toggles_the_column() {
    let mut h = harness(sequential(3));
    click(&mut h, "0");
    h.get_by_label("name").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    assert!(h.state().state.sort.is_empty(), "⌘-click must not sort");
    for row in 0..3 {
        assert!(h.state().state.is_selected(row, 1), "row {row}");
    }
    h.get_by_label("name").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    assert!(!h.state().state.is_selected(1, 1));
}

#[test]
fn command_click_again_removes_the_cell() {
    let mut h = harness(sequential(5));
    click(&mut h, "name 0");
    h.get_by_label("name 2").click_modifiers(Modifiers::COMMAND);
    h.get_by_label("name 2").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    assert!(h.state().state.is_selected(0, 1));
    assert!(!h.state().state.is_selected(2, 1));
}

#[test]
fn a_plain_click_clears_the_extra_selections() {
    let mut h = harness(sequential(5));
    click(&mut h, "name 0");
    h.get_by_label("name 2").click_modifiers(Modifiers::COMMAND);
    click(&mut h, "name 4");
    assert!(!h.state().state.is_selected(0, 1));
    assert!(h.state().state.is_selected(4, 1));
}

#[test]
fn copy_a_non_contiguous_selection_keeps_the_grid_shape() {
    let mut h = harness(sequential(5));
    click(&mut h, "name 0");
    h.get_by_label("1").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    h.event(Event::Copy);
    h.step();
    assert_eq!(copied(&h).as_deref(), Some("\tname 0\n1\t\n"));
}

#[test]
fn paste_one_cell_marks_it_pending() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    click(&mut h, "name 1");
    h.event(Event::Paste("renamed".into()));
    h.run_steps(2);
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.pending(), 1);
    assert_eq!(
        edits.value(&h.state().rows, 1, 1),
        Some(Value::Text("renamed".into()))
    );
    h.get_by_label("renamed");
}

#[test]
fn paste_a_block_into_a_multi_selection() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    click(&mut h, "0");
    press(&mut h, Modifiers::SHIFT, Key::ArrowDown);
    press(&mut h, Modifiers::SHIFT, Key::ArrowRight);
    h.event(Event::Paste("10\tten\n11\televen".into()));
    h.run_steps(2);
    let rows = &h.state().rows;
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.pending(), 2);
    assert_eq!(edits.value(rows, 0, 0), Some(Value::Int(10)));
    assert_eq!(edits.value(rows, 0, 1), Some(Value::Text("ten".into())));
    assert_eq!(edits.value(rows, 1, 0), Some(Value::Int(11)));
    assert_eq!(edits.value(rows, 1, 1), Some(Value::Text("eleven".into())));
}

#[test]
fn a_single_pasted_value_fills_the_selection() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    click(&mut h, "name 0");
    press(&mut h, Modifiers::SHIFT, Key::ArrowDown);
    h.event(Event::Paste("same".into()));
    h.run_steps(2);
    let rows = &h.state().rows;
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.pending(), 2);
    assert_eq!(edits.value(rows, 0, 1), Some(Value::Text("same".into())));
    assert_eq!(edits.value(rows, 1, 1), Some(Value::Text("same".into())));
}

#[test]
fn paste_into_a_read_only_grid_reports() {
    let mut h = harness(sequential(5));
    click(&mut h, "0");
    h.event(Event::Paste("x".into()));
    h.run_steps(2);
    assert!(h.state().actions.contains(&GridAction::PasteFailed(
        "The result cannot be edited".into()
    )));
}

#[test]
fn an_invalid_paste_reports_and_changes_nothing() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    click(&mut h, "0");
    h.event(Event::Paste("abc".into()));
    h.run_steps(2);
    assert_eq!(h.state().state.edits.as_ref().unwrap().pending(), 0);
    assert!(
        h.state()
            .actions
            .contains(&GridAction::PasteFailed("Not an integer".into()))
    );
}

#[test]
fn copy_as_markdown() {
    let mut h = harness(sequential(2));
    press(&mut h, Modifiers::NONE, Key::Escape);
    click(&mut h, "0");
    press(&mut h, Modifiers::COMMAND, Key::A);
    h.run_steps(100);
    let pos = center(&h, "name 1");
    press_at(&mut h, pos, PointerButton::Secondary, Modifiers::NONE);
    h.get_by_label_contains("Copy as").hover();
    h.run_steps(5);
    h.get_by_label("Markdown").click();
    h.run_steps(1);
    assert_eq!(
        copied(&h).as_deref(),
        Some("| id | name |\n| ---: | --- |\n| 0 | name 0 |\n| 1 | name 1 |\n")
    );
}

#[test]
fn header_click_sorts_complete_result() {
    let mut h = harness(buffer(&[Some(3), None, Some(1), Some(2)]));
    click(&mut h, "id");
    assert_eq!(h.state().state.sort, vec![(0, false)]);
    assert_eq!(h.state().state.view_rows(), vec![2, 3, 0, 1]);
    h.run_steps(100);
    click(&mut h, "id");
    // Descending, NULLs still last.
    assert_eq!(h.state().state.view_rows(), vec![0, 3, 2, 1]);
    h.run_steps(100);
    click(&mut h, "id");
    assert!(h.state().state.sort.is_empty());
    assert_eq!(h.state().state.view_rows(), vec![0, 1, 2, 3]);
    assert!(
        h.state()
            .actions
            .contains(&GridAction::Sort(vec![(0, true)]))
    );
    // Shift+click on a header selects the whole column instead.
    h.run_steps(100);
    h.get_by_label("name").click_modifiers(Modifiers::SHIFT);
    h.run_steps(2);
    let s = h.state().state.selection;
    assert_eq!((s.anchor, s.cursor), ((0, 1), (3, 1)));
    assert!(h.state().state.sort.is_empty());
}

#[test]
fn partial_result_offers_server_sort() {
    let mut h = harness_with(sequential(30), true, None);
    click(&mut h, "id");
    // Only the loaded rows would be sorted: nothing moves until the server sorts.
    assert_eq!(h.state().state.view_rows()[..3], [0, 1, 2]);
    h.get_by_label("Sort on server").click();
    h.run_steps(2);
    assert!(
        h.state()
            .actions
            .contains(&GridAction::SortOnServer(vec![(0, false)]))
    );
    assert!(h.query_by_label("Sort on server").is_none());
}

/// Appends rows `from..to` ("name i", id i) as one more chunk.
fn push_chunk(rows: &mut RowBuffer, from: usize, to: usize) {
    let mut b = ChunkBuilder::new(columns());
    for i in from..to {
        let name = format!("name {i}");
        b.push_row(&[Cell::Int(i as i64), Cell::Text(&name)])
            .unwrap();
    }
    rows.push_chunk(b.finish());
}

#[test]
fn where_field_filters_on_the_server() {
    let mut h = harness_with(sequential(5), true, None);
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_steps(2);
    h.event(Event::Text("id > 2".into()));
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(
        h.state()
            .actions
            .contains(&GridAction::FilterOnServer("id > 2".into()))
    );
}

#[test]
fn double_click_edits_cell_and_marks_pending() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    double_click(&mut h, "name 1");
    // The inline editor holds the value, selected.
    h.event(Event::Text("renamed".into()));
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(h.state().actions.contains(&GridAction::SetCell {
        row: 1,
        col: 1,
        value: Value::Text("renamed".into())
    }));
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.pending(), 1);
    assert_eq!(
        edits.value(&h.state().rows, 1, 1),
        Some(Value::Text("renamed".into()))
    );
    h.get_by_label("renamed");
    // The edited cell is tinted in the warning color.
    let tint = sqlr_ui::change_tint(sqlr_ui::theme::palette(&h.ctx).warning);
    assert!(has_fill(&h, tint), "no tinted cell");
}

#[test]
fn invalid_number_keeps_the_editor_open() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    double_click(&mut h, "2");
    h.event(Event::Text("abc".into()));
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(h.state().actions.is_empty(), "{:?}", h.state().actions);
    assert_eq!(h.state().state.edits.as_ref().unwrap().pending(), 0);
    h.get_by_label("Not an integer");
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert!(h.query_by_label("Not an integer").is_none());
}

#[test]
fn set_null_refused_on_not_null() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    h.run_steps(100);
    let pos = center(&h, "3");
    press_at(&mut h, pos, PointerButton::Secondary, Modifiers::NONE);
    assert!(h.get_by_label("Set NULL").accesskit_node().is_disabled());
    press(&mut h, Modifiers::NONE, Key::Escape);
    menu(&mut h, "name 3", "Set NULL");
    assert!(h.state().actions.contains(&GridAction::SetCell {
        row: 3,
        col: 1,
        value: Value::Null
    }));
}

#[test]
fn delete_row_strikes_through() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    menu(&mut h, "name 2", "Delete row");
    assert!(h.state().actions.contains(&GridAction::DeleteRows(vec![2])));
    assert_eq!(h.state().state.edits.as_ref().unwrap().pending(), 1);
    assert_eq!(struck(&h, "name 2"), Some(true));
    assert_eq!(struck(&h, "name 1"), Some(false));
    menu(&mut h, "name 2", "Revert row");
    assert_eq!(h.state().state.edits.as_ref().unwrap().pending(), 0);
    assert_eq!(struck(&h, "name 2"), Some(false));
}

#[test]
fn command_delete_marks_the_selected_rows_deleted() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    click(&mut h, "Row 2");
    press(&mut h, Modifiers::COMMAND, Key::Backspace);
    assert!(h.state().actions.contains(&GridAction::DeleteRows(vec![1])));
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.pending(), 1);
    assert_eq!(struck(&h, "name 1"), Some(true));
    assert_eq!(struck(&h, "name 2"), Some(false));
}

#[test]
fn command_delete_marks_a_multi_row_selection_deleted() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    click(&mut h, "Row 1");
    press(&mut h, Modifiers::SHIFT, Key::ArrowDown);
    press(&mut h, Modifiers::SHIFT, Key::ArrowDown);
    press(&mut h, Modifiers::COMMAND, Key::Backspace);
    assert!(
        h.state()
            .actions
            .contains(&GridAction::DeleteRows(vec![0, 1, 2]))
    );
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.pending(), 3);
    assert_eq!(struck(&h, "name 0"), Some(true));
    assert_eq!(struck(&h, "name 2"), Some(true));
    assert_eq!(struck(&h, "name 3"), Some(false));
}

#[test]
fn command_delete_deletes_non_contiguous_selected_rows() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    h.get_by_label("Row 1").click_modifiers(Modifiers::COMMAND);
    h.get_by_label("Row 3").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    press(&mut h, Modifiers::COMMAND, Key::Backspace);
    assert!(
        h.state()
            .actions
            .contains(&GridAction::DeleteRows(vec![0, 2]))
    );
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.pending(), 2);
    assert_eq!(struck(&h, "name 0"), Some(true));
    assert_eq!(struck(&h, "name 1"), Some(false));
    assert_eq!(struck(&h, "name 2"), Some(true));
}

#[test]
fn add_row_appends_an_inserted_row() {
    let mut h = harness_with(sequential(2), false, Some(editable()));
    menu(&mut h, "name 0", "Add row");
    assert!(h.state().actions.contains(&GridAction::AddRow));
    assert_eq!(h.state().state.edits.as_ref().unwrap().pending(), 1);
    assert_eq!(
        h.state().state.view_rows(),
        vec![0, 1, sqlr_ui::INSERTED_ROW_BASE]
    );
    // The new row's cells show their default.
    assert_eq!(h.get_all_by_label("DEFAULT").count(), 2);
    let tint = sqlr_ui::change_tint(sqlr_ui::theme::palette(&h.ctx).success);
    assert!(has_fill(&h, tint));
    menu(&mut h, "name 1", "Duplicate row");
    assert!(h.state().actions.contains(&GridAction::DuplicateRow(1)));
    assert_eq!(h.get_all_by_label("name 1").count(), 2);
}

#[test]
fn inserted_rows_keep_their_identity_when_more_rows_load() {
    let mut h = harness_with(sequential(2), true, Some(editable()));
    menu(&mut h, "name 0", "Add row");
    let inserted = *h.state().state.view_rows().last().unwrap();
    {
        let app = h.state_mut();
        app.state
            .set_cell(&app.rows, inserted, 1, Value::Text("new".into()))
            .unwrap();
    }
    push_chunk(&mut h.state_mut().rows, 2, 5);
    h.run_steps(2);
    let app = h.state();
    let edits = app.state.edits.as_ref().unwrap();
    // The new page goes before the inserted row, which keeps its number and value.
    assert_eq!(app.state.view_rows(), vec![0, 1, 2, 3, 4, inserted]);
    assert_eq!(
        edits.value(&app.rows, inserted, 1),
        Some(Value::Text("new".into()))
    );
    assert_eq!(
        edits.value(&app.rows, 2, 1),
        Some(Value::Text("name 2".into()))
    );
    assert_eq!(edits.pending(), 1);
    h.get_by_label("new");
    h.get_by_label("name 4");
}

#[test]
fn not_editable_explains_reason() {
    let mut h = Harness::builder()
        .with_size(vec2(600.0, 100.0))
        .build_ui(|ui| read_only_bar(ui, "no primary or unique key on orders"));
    h.run();
    h.get_by_label("Read-only: no primary or unique key on orders");

    let mut h = Harness::builder()
        .with_size(vec2(600.0, 100.0))
        .build_ui_state(
            |ui, action: &mut Option<ResultBarAction>| {
                let view = ResultBarView {
                    pending: 2,
                    applying: false,
                    editable: true,
                    exportable: true,
                    read_only: None,
                };
                if let Some(a) = result_bar(ui, &view) {
                    *action = Some(a);
                }
            },
            None,
        );
    h.run();
    h.get_by_label("2 pending changes");
    h.get_by_label("Apply").click();
    h.run();
    assert_eq!(
        *h.state(),
        Some(ResultBarAction::Changes(ChangesAction::Apply))
    );
}

fn has_fill(h: &Harness<'static, App>, color: egui::Color32) -> bool {
    fn visit(shape: &egui::Shape, color: egui::Color32) -> bool {
        match shape {
            egui::Shape::Rect(r) => r.fill == color,
            egui::Shape::Vec(shapes) => shapes.iter().any(|s| visit(s, color)),
            _ => false,
        }
    }
    h.output().shapes.iter().any(|c| visit(&c.shape, color))
}

fn has_wavy_underline<S>(h: &Harness<'static, S>, color: egui::Color32) -> bool {
    fn visit(shape: &egui::Shape, color: egui::Color32) -> bool {
        match shape {
            egui::Shape::Path(p) => {
                matches!(p.stroke.color, egui::epaint::ColorMode::Solid(c) if c == color)
                    && p.points.len() > 2
            }
            egui::Shape::Vec(shapes) => shapes.iter().any(|s| visit(s, color)),
            _ => false,
        }
    }
    h.output().shapes.iter().any(|c| visit(&c.shape, color))
}

#[test]
fn value_editor_checks_and_highlights_json_live() {
    use sqlr_ui::{ValueEdit, value_editor};
    let json = ColumnMeta::new("j", "jsonb", ColumnKind::Text, DisplayHint::Json);
    let edit = ValueEdit::new(0, 0, &json, &Value::Text("{\"a\": 1}".into()));
    let meta = json;
    let mut h = Harness::builder()
        .with_size(vec2(600.0, 400.0))
        .build_ui_state(
            move |ui, edit: &mut ValueEdit| {
                value_editor(ui, &meta, edit, false);
            },
            edit,
        );
    h.run_steps(2);
    // The JSON value editor is the query editor, in the JSON language.
    h.get_by_label("JSON editor");
    h.get_by_label("JSON editor").click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(Event::Text("{oops".into()));
    h.run_steps(2);
    assert_eq!(h.state().text, "{oops");
    assert!(
        has_wavy_underline(&h, sqlr_ui::theme::palette(&h.ctx).error),
        "invalid JSON is underlined"
    );
    // "se" is a SQL keyword prefix; a JSON value must not offer SQL completion.
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(Event::Text("se".into()));
    h.run_steps(2);
    assert!(
        h.query_by_label("SELECT").is_none(),
        "a JSON value offers no SQL completion"
    );

    h.state_mut().set_text("{\"a\": [1, 2]}");
    h.run_steps(2);
    assert!(
        !has_wavy_underline(&h, sqlr_ui::theme::palette(&h.ctx).error),
        "valid JSON has no underline"
    );
}

#[test]
fn value_editor_beautifies_json() {
    use sqlr_ui::{ValueEdit, ValueEditAction, value_editor};
    let pretty = "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ]\n}";
    let json = ColumnMeta::new("j", "jsonb", ColumnKind::Text, DisplayHint::Json);
    let edit = ValueEdit::new(0, 0, &json, &Value::Text("{\"a\":1}".into()));
    let meta = json;
    let mut h = Harness::builder()
        .with_size(vec2(600.0, 400.0))
        .build_ui_state(
            move |ui, (edit, out): &mut (ValueEdit, Option<ValueEditAction>)| {
                if let Some(a) = value_editor(ui, &meta, edit, false) {
                    *out = Some(a);
                }
            },
            (edit, None),
        );
    h.run_steps(2);
    h.state_mut().0.set_text("{\"a\":1,\"b\":[1,2]}");
    h.run_steps(2);
    h.get_by_label("Beautify").click();
    h.run_steps(2);
    assert_eq!(h.state().0.text, pretty);
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert_eq!(
        h.state().1,
        Some(ValueEditAction::Save(Value::Text(pretty.into())))
    );

    // The reformat is one undoable edit.
    h.get_by_label("JSON editor").click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.run_steps(2);
    assert_eq!(h.state().0.text, "{\"a\":1,\"b\":[1,2]}");

    // Invalid JSON is left as it is and reports why.
    h.state_mut().0.set_text("{oops");
    h.run_steps(2);
    h.get_by_label("Beautify").click();
    h.run_steps(2);
    assert_eq!(h.state().0.text, "{oops");
    h.get_by_label_contains("Invalid JSON");
}

#[test]
fn value_editor_validates_json_and_hex() {
    use sqlr_ui::{ValueEdit, ValueEditAction, value_editor};
    let json = ColumnMeta::new("j", "jsonb", ColumnKind::Text, DisplayHint::Json);
    let edit = ValueEdit::new(0, 0, &json, &Value::Text("{\"a\":1}".into()));
    assert_eq!(edit.text, "{\n  \"a\": 1\n}");
    let meta = json.clone();
    let mut h = Harness::builder()
        .with_size(vec2(500.0, 400.0))
        .build_ui_state(
            move |ui, (edit, out): &mut (ValueEdit, Option<ValueEditAction>)| {
                if let Some(a) = value_editor(ui, &meta, edit, false) {
                    *out = Some(a);
                }
            },
            (edit, None),
        );
    h.run();
    h.state_mut().0.set_text("{oops");
    h.get_by_label("Save").click();
    h.run();
    assert_eq!(h.state().1, None);
    h.get_by_label_contains("Invalid JSON");
    h.state_mut().0.set_text("[1, 2]");
    h.get_by_label("Save").click();
    h.run();
    assert_eq!(
        h.state().1,
        Some(ValueEditAction::Save(Value::Text("[1, 2]".into())))
    );

    let bytes = ColumnMeta::new("b", "bytea", ColumnKind::Bytes, DisplayHint::Binary);
    let mut edit = ValueEdit::new(3, 1, &bytes, &Value::Bytes(vec![1, 0xab]));
    assert_eq!(edit.text, "0x01ab");
    assert_eq!(edit.save(&bytes), Ok(Value::Bytes(vec![1, 0xab])));
    edit.text = "0x1".into();
    assert_eq!(
        edit.save(&bytes).unwrap_err().to_string(),
        "Not hex bytes: 0x followed by pairs of hex digits"
    );
    let _ = &json;
}

/// Whether the painted text `text` is struck through; `None` when it is not painted.
fn struck(h: &Harness<'static, App>, text: &str) -> Option<bool> {
    fn visit(shape: &egui::Shape, text: &str, found: &mut Option<bool>) {
        match shape {
            egui::Shape::Text(t) if t.galley.text() == text => {
                *found = Some(
                    t.galley
                        .job
                        .sections
                        .iter()
                        .all(|s| s.format.strikethrough.width > 0.0),
                );
            }
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| visit(s, text, found)),
            _ => {}
        }
    }
    let mut found = None;
    for c in &h.output().shapes {
        visit(&c.shape, text, &mut found);
    }
    found
}

#[test]
fn pasted_blocks_tile_and_undo_as_one_operation() {
    let mut h = harness_with(sequential(6), false, Some(editable()));
    click(&mut h, "name 0");
    for _ in 0..3 {
        press(&mut h, Modifiers::SHIFT, Key::ArrowDown);
    }
    h.event(Event::Paste("alpha\nbeta".into()));
    h.run_steps(2);
    for row in 0..4 {
        assert_eq!(
            h.state()
                .state
                .edits
                .as_ref()
                .unwrap()
                .value(&h.state().rows, row, 1),
            Some(Value::Text(
                if row % 2 == 0 { "alpha" } else { "beta" }.into()
            ))
        );
    }
    press(&mut h, Modifiers::COMMAND, Key::Z);
    assert_eq!(h.state().state.edits.as_ref().unwrap().pending(), 0);
    press(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    assert_eq!(h.state().state.edits.as_ref().unwrap().pending(), 4);
}

#[test]
fn pasted_block_repeats_at_each_disjoint_anchor_with_legacy_fallback() {
    let mut h = harness_with(sequential(6), false, Some(editable()));
    click(&mut h, "name 0");
    h.get_by_label("name 3").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    h.event(Event::Paste("alpha\nbeta".into()));
    h.run_steps(2);
    for (row, text) in [(0, "alpha"), (1, "beta"), (3, "alpha"), (4, "beta")] {
        assert_eq!(
            h.state()
                .state
                .edits
                .as_ref()
                .unwrap()
                .value(&h.state().rows, row, 1),
            Some(Value::Text(text.into()))
        );
    }
    assert_eq!(h.state().state.edits.as_ref().unwrap().pending(), 4);
}

#[test]
fn set_null_and_duplicate_apply_to_all_selected_rows() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    click(&mut h, "name 0");
    h.get_by_label("name 2").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    menu(&mut h, "name 2", "Set NULL");
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.value(&h.state().rows, 0, 1), Some(Value::Null));
    assert_eq!(edits.value(&h.state().rows, 2, 1), Some(Value::Null));
    press(&mut h, Modifiers::COMMAND, Key::Z);
    menu(&mut h, "name 2", "Duplicate rows");
    assert_eq!(h.state().state.edits.as_ref().unwrap().inserted_rows(), 2);
    press(&mut h, Modifiers::COMMAND, Key::Z);
    assert_eq!(h.state().state.edits.as_ref().unwrap().inserted_rows(), 0);
}

#[test]
fn revert_selected_cells_preserves_other_edits_in_the_row() {
    let mut h = harness_with(sequential(3), false, Some(editable()));
    click(&mut h, "0");
    h.event(Event::Paste("42\tchanged".into()));
    h.run_steps(2);
    menu(&mut h, "changed", "Revert selected cells");
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.value(&h.state().rows, 0, 0), Some(Value::Int(42)));
    assert_eq!(
        edits.value(&h.state().rows, 0, 1),
        Some(Value::Text("name 0".into()))
    );
    press(&mut h, Modifiers::COMMAND, Key::Z);
    assert_eq!(
        h.state()
            .state
            .edits
            .as_ref()
            .unwrap()
            .value(&h.state().rows, 0, 1),
        Some(Value::Text("changed".into()))
    );
}

#[test]
fn typed_copy_does_not_paste_unselected_holes_as_null() {
    let mut h = harness_with(sequential(5), false, Some(editable()));
    click(&mut h, "0");
    h.get_by_label("name 1").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    h.event(Event::Copy);
    h.step();
    let text = copied(&h).unwrap();
    click(&mut h, "2");
    h.event(Event::Paste(text));
    h.run_steps(2);
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.value(&h.state().rows, 2, 0), Some(Value::Int(0)));
    assert_eq!(
        edits.value(&h.state().rows, 2, 1),
        Some(Value::Text("name 2".into()))
    );
    assert_eq!(edits.value(&h.state().rows, 3, 0), Some(Value::Int(3)));
    assert_eq!(
        edits.value(&h.state().rows, 3, 1),
        Some(Value::Text("name 1".into()))
    );
}

#[test]
fn paste_as_new_rows_matches_names_and_can_be_undone() {
    let mut h = harness_with(sequential(4), false, Some(editable()));
    click(&mut h, "name 0");
    press(&mut h, Modifiers::SHIFT, Key::ArrowDown);
    h.event(Event::Copy);
    h.step();
    let text = copied(&h).unwrap();
    menu(&mut h, "name 1", "Paste as new rows…");
    h.event(Event::Paste(text));
    h.run_steps(2);
    h.get_by_label("Add pending rows").click();
    h.run_steps(3);
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.inserted_rows(), 2);
    for (i, row) in edits.inserted_row_numbers().into_iter().enumerate() {
        assert_eq!(edits.value(&h.state().rows, row, 0), None);
        assert_eq!(
            edits.value(&h.state().rows, row, 1),
            Some(Value::Text(format!("name {i}")))
        );
    }
    press(&mut h, Modifiers::COMMAND, Key::Z);
    assert_eq!(h.state().state.edits.as_ref().unwrap().inserted_rows(), 0);
    press(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    assert_eq!(h.state().state.edits.as_ref().unwrap().inserted_rows(), 2);
}

#[test]
fn an_invalid_cell_in_a_later_paste_area_leaves_every_area_unchanged() {
    let mut h = harness_with(sequential(4), false, Some(editable()));
    click(&mut h, "name 0");
    h.get_by_label("2").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    h.event(Event::Paste("one\ntwo".into()));
    h.run_steps(2);
    assert_eq!(h.state().state.edits.as_ref().unwrap().pending(), 0);
    assert!(!h.state().state.can_undo());
}

#[test]
fn a_new_edit_clears_redo_and_empty_history_does_not_remove_editability() {
    let mut h = harness_with(sequential(4), false, Some(editable()));
    h.state_mut().state.undo();
    assert!(h.state().state.edits.is_some());
    click(&mut h, "name 0");
    h.event(Event::Paste("first".into()));
    h.run_steps(2);
    press(&mut h, Modifiers::COMMAND, Key::Z);
    assert!(h.state().state.can_redo());
    h.event(Event::Paste("second".into()));
    h.run_steps(2);
    assert!(!h.state().state.can_redo());
}

#[test]
fn typed_paste_into_an_empty_grid_adds_rows_without_overwriting_generated_columns() {
    let mut h = harness_with(sequential(0), false, Some(editable()));
    let text = "name\tid\nAda\t42\n";
    sqlr_ui::set_grid_clipboard(
        &h.ctx,
        Arc::new(sqlr_core::GridClipboard {
            columns: vec![columns()[1].clone(), columns()[0].clone()],
            rows: vec![vec![Some(Value::Text("Ada".into())), Some(Value::Int(42))]],
            text: text.into(),
        }),
    );
    h.get_by_label("Paste as new rows…").click();
    h.run_steps(2);
    h.event(Event::Paste(text.into()));
    h.run_steps(2);
    h.get_by_label("Add pending rows").click();
    h.run_steps(3);
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.inserted_rows(), 1);
    let row = edits.inserted_row_numbers()[0];
    assert_eq!(edits.value(&h.state().rows, row, 0), None);
    assert_eq!(
        edits.value(&h.state().rows, row, 1),
        Some(Value::Text("Ada".into()))
    );
}

#[test]
fn typed_copy_keeps_null_empty_text_and_binary_values_distinct() {
    let cols: Arc<[ColumnMeta]> = Arc::from(vec![ColumnMeta::new(
        "value",
        "blob",
        ColumnKind::Mixed,
        DisplayHint::Plain,
    )]);
    let mut b = ChunkBuilder::new(cols.clone());
    for value in [
        Cell::Null,
        Cell::Text(""),
        Cell::Bytes(&[0, 255]),
        Cell::Text("NULL"),
        Cell::Text("=1+1"),
    ] {
        b.push_row(&[value]).unwrap();
    }
    let mut rows = RowBuffer::new(cols);
    rows.push_chunk(b.finish());
    let mut h = harness_with(
        rows,
        false,
        Some(EditBuffer::new(vec![], vec![true], vec![false])),
    );
    h.get_by_label("Row 1").click();
    h.run_steps(2);
    for source in 0..5 {
        h.state_mut().state.selection = sqlr_ui::Selection::cell(source, 0);
        h.state_mut().state.selected = Some((source, 0));
        h.run_steps(2);
        h.event(Event::Copy);
        h.step();
        let text = copied(&h).unwrap();
        h.state_mut().state.selection = sqlr_ui::Selection::cell(0, 0);
        h.state_mut().state.selected = Some((0, 0));
        h.event(Event::Paste(text));
        h.run_steps(2);
        assert_eq!(
            h.state()
                .state
                .edits
                .as_ref()
                .unwrap()
                .value(&h.state().rows, 0, 0),
            h.state().rows.cell(source, 0).map(Value::from_cell)
        );
    }
}

#[test]
fn a_large_new_row_paste_is_prepared_in_background_and_undoes_once() {
    let mut h = harness_with(sequential(0), false, Some(editable()));
    let text = "large typed clipboard";
    sqlr_ui::set_grid_clipboard(
        &h.ctx,
        Arc::new(sqlr_core::GridClipboard {
            columns: vec![columns()[1].clone()],
            rows: (0..1500)
                .map(|i| vec![Some(Value::Text(format!("entry {i}")))])
                .collect(),
            text: text.into(),
        }),
    );
    h.get_by_label("Paste as new rows…").click();
    h.run_steps(2);
    h.event(Event::Paste(text.into()));
    h.run_steps(2);
    h.get_by_label("Add pending rows").click();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while h.state().state.edits.as_ref().unwrap().inserted_rows() == 0 {
        h.step();
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    h.run_steps(2);
    let edits = h.state().state.edits.as_ref().unwrap();
    assert_eq!(edits.inserted_rows(), 1500);
    assert_eq!(
        edits.value(
            &h.state().rows,
            *edits.inserted_row_numbers().last().unwrap(),
            1
        ),
        Some(Value::Text("entry 1499".into()))
    );
    press(&mut h, Modifiers::COMMAND, Key::Z);
    assert_eq!(h.state().state.edits.as_ref().unwrap().inserted_rows(), 0);
}

#[test]
fn read_only_result_can_open_its_table() {
    let mut h = Harness::builder().with_size((900.0, 100.0)).build_ui_state(
        |ui, action: &mut Option<ResultBarAction>| {
            *action = result_bar(
                ui,
                &ResultBarView {
                    pending: 0,
                    applying: false,
                    editable: false,
                    exportable: true,
                    read_only: Some("columns from several tables"),
                },
            );
        },
        None,
    );
    h.run_steps(2);
    h.get_by_label("Open table to edit").click();
    h.run_steps(1);
    assert_eq!(format!("{:?}", h.state()), "Some(OpenTable)");
}
