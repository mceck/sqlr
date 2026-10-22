use std::sync::Arc;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_core::{Cell, ChunkBuilder, ColumnKind, ColumnMeta, DisplayHint, RowBuffer};
use sqlr_ui::{GridOutput, GridState, MAX_CELL_CHARS, cell_display, result_grid};

fn columns() -> Arc<[ColumnMeta]> {
    Arc::from(vec![
        ColumnMeta::new("id", "int8", ColumnKind::Int, DisplayHint::Number),
        ColumnMeta::new("name", "text", ColumnKind::Text, DisplayHint::Plain),
    ])
}

fn buffer(rows: usize, chunk_rows: usize) -> RowBuffer {
    let cols = columns();
    let mut buf = RowBuffer::new(cols.clone());
    let mut next = 0;
    while next < rows {
        let end = (next + chunk_rows).min(rows);
        let mut b = ChunkBuilder::new(cols.clone());
        for i in next..end {
            let name = format!("name {i}");
            b.push_row(&[Cell::Int(i as i64), Cell::Text(&name)])
                .unwrap();
        }
        buf.push_chunk(b.finish());
        next = end;
    }
    buf
}

struct App {
    state: GridState,
    rows: RowBuffer,
    more: bool,
    out: GridOutput,
}

fn harness(rows: usize, more: bool) -> Harness<'static, App> {
    let app = App {
        state: GridState::default(),
        rows: buffer(rows, 500),
        more,
        out: GridOutput {
            need_more: false,
            selection_changed: false,
        },
    };
    let mut h = Harness::builder()
        .with_size(vec2(1000.0, 600.0))
        .build_ui_state(
            |ui, app: &mut App| {
                app.out = result_grid(ui, &mut app.state, &app.rows, app.more);
            },
            app,
        );
    h.run_steps(2);
    h
}

fn click(h: &mut Harness<'static, App>, pos: Pos2) {
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run_steps(2);
}

#[test]
fn cell_display_rules() {
    assert_eq!(cell_display(Cell::Null), "NULL");
    assert_eq!(cell_display(Cell::Text("a\nb")), "a⏎b");
    assert_eq!(cell_display(Cell::Int(5)), "5");
}

#[test]
fn long_multiline_cell_is_truncated_single_line() {
    let text = "x".repeat(5000) + "\nmore";
    let shown = cell_display(Cell::Text(&text));
    assert_eq!(shown.chars().count(), MAX_CELL_CHARS + 1);
    assert!(shown.ends_with('…'));
    assert!(!shown.contains('\n'));
    // Multi-byte characters never split.
    let emoji = "😀".repeat(1000);
    assert_eq!(
        cell_display(Cell::Text(&emoji)).chars().count(),
        MAX_CELL_CHARS + 1
    );
}

#[test]
fn renders_only_visible_rows() {
    let mut h = Harness::builder()
        .with_size(vec2(1000.0, 600.0))
        .build_ui_state(
            |ui, app: &mut App| {
                app.out = result_grid(ui, &mut app.state, &app.rows, app.more);
            },
            App {
                state: GridState::default(),
                rows: buffer(100_000, 500),
                more: false,
                out: GridOutput {
                    need_more: false,
                    selection_changed: false,
                },
            },
        );
    h.step();
    let visible = &h.state().state.last_visible_rows;
    assert!(visible.len() < 60, "{visible:?}");
    assert_eq!(visible.start, 0);
}

#[test]
fn need_more_near_end() {
    let mut h = harness(120, true);
    assert!(!h.state().out.need_more);
    h.state_mut().state.scroll_to_row = Some(119);
    h.run_steps(2);
    assert!(h.state().out.need_more);
}

#[test]
fn no_need_more_when_complete() {
    let mut h = harness(120, false);
    h.state_mut().state.scroll_to_row = Some(119);
    h.run_steps(2);
    assert!(!h.state().out.need_more);
}

/// The center of the first data cell (id 0), right of the row numbers.
fn first_cell_pos(h: &Harness<'static, App>) -> Pos2 {
    h.get_by_label("0").rect().center()
}

#[test]
fn click_selects_cell() {
    let mut h = harness(50, false);
    let pos = first_cell_pos(&h);
    h.state_mut().out.selection_changed = false;
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    // `selection_changed` is reported on the frame that handles the click.
    h.step();
    assert!(h.state().out.selection_changed);
    assert_eq!(h.state().state.selected, Some((0, 0)));
}

#[test]
fn arrow_moves_selection() {
    let mut h = harness(50, false);
    let pos = first_cell_pos(&h);
    click(&mut h, pos);
    assert_eq!(h.state().state.selected, Some((0, 0)));
    h.key_press(Key::ArrowDown);
    h.run_steps(2);
    h.key_press(Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(h.state().state.selected, Some((1, 1)));
}

#[test]
fn copy_event_copies_selected_cell_text() {
    let mut h = harness(50, false);
    let pos = first_cell_pos(&h);
    click(&mut h, pos);
    h.event(Event::Copy);
    h.step();
    let copied = h
        .output()
        .platform_output
        .commands
        .iter()
        .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == "0"));
    assert!(copied);
}

#[test]
fn grid_renders_bytes_and_null_italic() {
    assert_eq!(cell_display(Cell::Bytes(&[1, 2])), "0x0102… (2 bytes)");
    let long: Vec<u8> = (0..40).collect();
    let shown = cell_display(Cell::Bytes(&long));
    assert_eq!(
        shown,
        format!(
            "0x{}… (40 bytes)",
            "00010203040506070809".to_string()
                + &(10..32u8).map(|b| format!("{b:02x}")).collect::<String>()
        )
    );

    let cols: Arc<[ColumnMeta]> = Arc::from(vec![
        ColumnMeta::new("b", "bytea", ColumnKind::Bytes, DisplayHint::Binary),
        ColumnMeta::new("n", "text", ColumnKind::Text, DisplayHint::Plain),
        ColumnMeta::new("j", "jsonb", ColumnKind::Text, DisplayHint::Json),
    ]);
    let mut rows = RowBuffer::new(cols.clone());
    let mut b = ChunkBuilder::new(cols);
    b.push_row(&[
        Cell::Bytes(&[1, 2]),
        Cell::Null,
        Cell::Text("{\n  \"a\": 1\n}"),
    ])
    .unwrap();
    rows.push_chunk(b.finish());
    let mut h = Harness::builder()
        .with_size(vec2(1000.0, 600.0))
        .build_ui_state(
            |ui, app: &mut App| {
                app.out = result_grid(ui, &mut app.state, &app.rows, app.more);
            },
            App {
                state: GridState::default(),
                rows,
                more: false,
                out: GridOutput {
                    need_more: false,
                    selection_changed: false,
                },
            },
        );
    h.run_steps(2);
    h.get_by_label("0x0102… (2 bytes)");
    h.get_by_label("NULL");
    // JSON is compacted to one line.
    h.get_by_label("{\"a\":1}");
    // NULL is painted in italics.
    let mut null_italic = None;
    for clipped in &h.output().shapes {
        visit_text(&clipped.shape, &mut |galley| {
            if galley.text() == "NULL" {
                null_italic = Some(galley.job.sections.iter().all(|s| s.format.italics));
            }
        });
    }
    assert_eq!(null_italic, Some(true));
}

fn visit_text(shape: &egui::Shape, f: &mut dyn FnMut(&egui::Galley)) {
    match shape {
        egui::Shape::Text(t) => f(&t.galley),
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| visit_text(s, f)),
        _ => {}
    }
}

#[test]
fn grid_json_keeps_numbers_as_written() {
    let cols: Arc<[ColumnMeta]> = Arc::from(vec![ColumnMeta::new(
        "j",
        "jsonb",
        ColumnKind::Text,
        DisplayHint::Json,
    )]);
    let mut rows = RowBuffer::new(cols.clone());
    let mut b = ChunkBuilder::new(cols);
    b.push_row(&[Cell::Text(
        "{\"n\": 123456789012345678901234,\n \"d\": 1.10}",
    )])
    .unwrap();
    rows.push_chunk(b.finish());
    let mut h = Harness::builder()
        .with_size(vec2(1000.0, 600.0))
        .build_ui_state(
            |ui, app: &mut App| {
                app.out = result_grid(ui, &mut app.state, &app.rows, app.more);
            },
            App {
                state: GridState::default(),
                rows,
                more: false,
                out: GridOutput {
                    need_more: false,
                    selection_changed: false,
                },
            },
        );
    h.run_steps(2);
    h.get_by_label("{\"n\":123456789012345678901234,\"d\":1.10}");
}
