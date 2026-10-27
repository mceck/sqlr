use std::collections::HashMap;

use egui::accesskit::Role;
use egui::{Event, ImeEvent, Key, Modifiers, OutputCommand, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_core::{
    CatalogNeed, CatalogRelation, QueryLanguage, RelationKind, SchemaCatalog, SqlSyntax,
};

use sqlr_ui::{Caret, CompletionSource, EditorState, Marker, Pos, RunRequest, query_editor};

#[derive(Default)]
struct StubSource {
    available: bool,
    schemas: Option<Vec<String>>,
    catalogs: HashMap<String, SchemaCatalog>,
    defaults: Vec<String>,
    /// Every need the editor emitted, in order.
    requested: Vec<CatalogNeed>,
}

impl CompletionSource for StubSource {
    fn available(&self) -> bool {
        self.available
    }

    fn schemas(&self) -> Option<&[String]> {
        self.schemas.as_deref()
    }

    fn schema(&self, name: &str) -> Option<&SchemaCatalog> {
        self.catalogs.get(name)
    }

    fn default_schemas(&self) -> &[String] {
        &self.defaults
    }
}

type State = (EditorState, StubSource);
type H = Harness<'static, State>;

#[test]
fn another_edit_moves_an_agent_highlight_without_panicking() {
    let mut editor = EditorState::new("SELECT 1", SqlSyntax::ansi());
    editor.highlight_agent_edit(7);
    editor.replace_ranges(&[(0..0, "--\n".into())]);
    assert_eq!(editor.text(), "--\nSELECT 1");
    assert_eq!(editor.markers()[0].at, 10);
    assert_eq!(editor.markers()[0].message, "Agent edit");
    editor.replace_all("SELECT 2");
    assert_eq!(editor.text(), "SELECT 2");
    assert!(editor.markers().is_empty());
}

fn harness_with(text: &str, source: StubSource) -> H {
    let mut h = Harness::builder()
        .with_size(vec2(900.0, 600.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, s: &mut State| {
                let out = query_editor(ui, &mut s.0, &s.1);
                s.1.requested.extend(out.needs);
            },
            (EditorState::new(text, SqlSyntax::ansi()), source),
        );
    h.run_steps(2);
    h
}

fn harness(text: &str) -> H {
    harness_with(text, StubSource::default())
}

/// A harness of `size`, for tests that need a narrow editor (soft wrap).
fn harness_sized(text: &str, size: egui::Vec2) -> H {
    let mut h = Harness::builder()
        .with_size(size)
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, s: &mut State| {
                let out = query_editor(ui, &mut s.0, &s.1);
                s.1.requested.extend(out.needs);
            },
            (
                EditorState::new(text, SqlSyntax::ansi()),
                StubSource::default(),
            ),
        );
    h.run_steps(2);
    h
}

/// Like `harness_sized`, with soft wrap on before the very first frame.
fn harness_wrapped(text: &str, size: egui::Vec2) -> H {
    let mut state = EditorState::new(text, SqlSyntax::ansi());
    state.set_soft_wrap(true);
    let mut h = Harness::builder()
        .with_size(size)
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, s: &mut State| {
                let out = query_editor(ui, &mut s.0, &s.1);
                s.1.requested.extend(out.needs);
            },
            (state, StubSource::default()),
        );
    h.run_steps(2);
    h
}

fn p(line: usize, col: usize) -> Pos {
    Pos { line, col }
}

fn screen(h: &H, at: Pos) -> Pos2 {
    h.state()
        .0
        .screen_pos(at)
        .unwrap_or_else(|| panic!("{at:?} is not visible"))
}

fn click_at(h: &mut H, pos: Pos2, modifiers: Modifiers) {
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers,
        });
    }
    h.run_steps(2);
}

fn click_pos(h: &mut H, at: Pos, modifiers: Modifiers) {
    let pos = screen(h, at);
    click_at(h, pos, modifiers);
}

fn type_text(h: &mut H, s: &str) {
    h.event(Event::Text(s.into()));
    h.run_steps(2);
}

fn key(h: &mut H, modifiers: Modifiers, k: Key) {
    h.key_press_modifiers(modifiers, k);
    h.run_steps(2);
}

fn text(h: &H) -> String {
    h.state().0.text()
}
fn primary(h: &H) -> Caret {
    h.state().0.model().primary()
}

#[test]
fn typing_inserts_at_caret() {
    let mut h = harness("select ");
    h.get_by_label("SQL editor");
    click_pos(&mut h, p(0, 7), Modifiers::NONE);
    assert!(h.state().0.has_focus());
    type_text(&mut h, "1");
    assert_eq!(text(&h), "select 1");
    key(&mut h, Modifiers::NONE, Key::Enter);
    type_text(&mut h, "x");
    assert_eq!(text(&h), "select 1\nx");
}

#[test]
fn click_places_caret_and_shift_click_selects() {
    let mut h = harness("hello world");
    click_pos(&mut h, p(0, 2), Modifiers::NONE);
    assert_eq!(primary(&h), Caret::at(p(0, 2)));
    click_pos(&mut h, p(0, 8), Modifiers::SHIFT);
    assert_eq!(
        primary(&h),
        Caret {
            anchor: p(0, 2),
            head: p(0, 8)
        }
    );
    assert_eq!(h.state().0.model().selected_text(), "llo wo");
}

#[test]
fn alt_click_adds_caret_and_typing_hits_both() {
    let mut h = harness("aa\nbb");
    click_pos(&mut h, p(0, 2), Modifiers::NONE);
    click_pos(&mut h, p(1, 2), Modifiers::ALT);
    assert_eq!(h.state().0.model().carets().len(), 2);
    type_text(&mut h, "!");
    assert_eq!(text(&h), "aa!\nbb!");
}

#[test]
fn copy_emits_platform_copy() {
    let mut h = harness("abc def");
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    h.state_mut().0.set_carets(vec![Caret {
        anchor: p(0, 0),
        head: p(0, 3),
    }]);
    h.event(Event::Copy);
    h.step();
    assert!(
        h.output()
            .platform_output
            .commands
            .contains(&OutputCommand::CopyText("abc".into())),
        "{:?}",
        h.output().platform_output.commands
    );
    assert_eq!(text(&h), "abc def");
    h.event(Event::Cut);
    h.step();
    assert_eq!(text(&h), " def");
}

#[test]
fn escape_keeps_focus() {
    let mut h = harness("aa\nbb");
    click_pos(&mut h, p(0, 1), Modifiers::NONE);
    click_pos(&mut h, p(1, 1), Modifiers::ALT);
    key(&mut h, Modifiers::NONE, Key::Escape);
    assert!(h.state().0.has_focus());
    assert_eq!(
        h.state().0.model().carets().len(),
        1,
        "Esc collapses carets"
    );
    key(&mut h, Modifiers::NONE, Key::Escape);
    assert!(h.state().0.has_focus());
}

#[test]
fn escape_clears_the_selection_last() {
    let mut h = harness("abc def");
    click_pos(&mut h, p(0, 1), Modifiers::NONE);
    key(&mut h, Modifiers::SHIFT, Key::ArrowRight);
    key(&mut h, Modifiers::SHIFT, Key::ArrowRight);
    assert!(h.state().0.escape_would_clear(), "a selection to clear");
    key(&mut h, Modifiers::NONE, Key::Escape);
    let carets = h.state().0.model().carets().to_vec();
    assert_eq!(carets.len(), 1);
    assert_eq!(carets[0].head, carets[0].anchor, "Esc clears the selection");
    assert_eq!(carets[0].head, p(0, 3), "the caret stays at the head");
    assert!(!h.state().0.escape_would_clear());
}

#[test]
fn ime_preedit_then_commit() {
    let mut h = harness("");
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    h.event(Event::Ime(ImeEvent::Preedit {
        text: "に".into(),
        active_range_chars: None,
    }));
    h.run_steps(2);
    assert_eq!(text(&h), "");
    assert!(h.output().platform_output.ime.is_some());
    h.event(Event::Ime(ImeEvent::Commit("日本".into())));
    h.run_steps(2);
    assert_eq!(text(&h), "日本");
}

#[test]
fn renders_only_visible_lines() {
    let big = "select 1;\n".repeat(200_000);
    let h = harness(&big);
    let stats = h.state().0.frame_stats();
    assert!(
        stats.visible_lines > 0 && stats.visible_lines < 60,
        "{stats:?}"
    );
}

#[test]
fn command_enter_is_not_a_newline() {
    let mut h = harness("select 1");
    click_pos(&mut h, p(0, 8), Modifiers::NONE);
    key(&mut h, Modifiers::COMMAND, Key::Enter);
    assert_eq!(text(&h), "select 1");
}

#[test]
fn undo_redo_keys() {
    let mut h = harness("");
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    type_text(&mut h, "abc");
    key(&mut h, Modifiers::COMMAND, Key::Z);
    assert_eq!(text(&h), "");
    key(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    assert_eq!(text(&h), "abc");
    key(&mut h, Modifiers::COMMAND, Key::Z);
    key(&mut h, Modifiers::COMMAND, Key::Y);
    assert_eq!(text(&h), "abc");
}

#[test]
fn toggle_comment_key() {
    let mut h = harness("select 1");
    click_pos(&mut h, p(0, 3), Modifiers::NONE);
    key(&mut h, Modifiers::COMMAND, Key::Slash);
    assert_eq!(text(&h), "-- select 1");
    key(&mut h, Modifiers::COMMAND, Key::Slash);
    assert_eq!(text(&h), "select 1");
}

#[test]
fn toggle_comment_does_nothing_in_a_redis_console() {
    // Redis has no comment syntax: ⌘/ must not put `--` in front of a command.
    let mut h = harness("GET key");
    h.state_mut()
        .0
        .set_language(sqlr_core::QueryLanguage::Redis);
    h.run_steps(2);
    click_pos(&mut h, p(0, 3), Modifiers::NONE);
    key(&mut h, Modifiers::COMMAND, Key::Slash);
    assert_eq!(text(&h), "GET key");
    // The other languages keep theirs.
    h.state_mut()
        .0
        .set_language(sqlr_core::QueryLanguage::MongoShell);
    h.run_steps(2);
    key(&mut h, Modifiers::COMMAND, Key::Slash);
    assert_eq!(text(&h), "// GET key");
}

#[test]
fn find_bar_next_and_replace_all() {
    let mut h = harness("x abc y abc");
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    key(&mut h, Modifiers::COMMAND, Key::F);
    h.get_by_role_and_label(Role::TextInput, "Find")
        .type_text("abc");
    h.run_steps(2);
    key(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(
        primary(&h),
        Caret {
            anchor: p(0, 2),
            head: p(0, 5)
        }
    );
    key(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(primary(&h).range(), (p(0, 8), p(0, 11)));

    key(&mut h, Modifiers::COMMAND | Modifiers::ALT, Key::F);
    h.get_by_role_and_label(Role::TextInput, "Replace with")
        .focus();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Replace with")
        .type_text("x");
    h.run_steps(2);
    h.get_by_label("Replace all").click();
    h.run_steps(2);
    assert_eq!(text(&h), "x x y x");

    h.get_by_role_and_label(Role::TextInput, "Find").focus();
    h.run_steps(2);
    key(&mut h, Modifiers::NONE, Key::Escape);
    assert!(
        h.query_by_label("Replace all").is_none(),
        "Esc closes the bar"
    );
}

#[test]
fn marker_maps_char_offset_after_emoji() {
    let mut h = harness("select '🚀', frm");
    h.state_mut().0.set_markers(vec![Marker {
        at: 12,
        message: "syntax error at or near \"frm\"".into(),
    }]);
    h.run_steps(2);
    assert_eq!(h.state().0.frame_stats().painted_markers, 1);
    assert_eq!(h.state().0.last_marker_spans(), vec![12..15]);
}

#[test]
fn markers_shift_and_drop() {
    let mut h = harness("select frm");
    h.state_mut().0.set_markers(vec![Marker {
        at: 7,
        message: "boom".into(),
    }]);
    h.run_steps(2);
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    type_text(&mut h, "x");
    assert_eq!(h.state().0.markers()[0].at, 8);
    assert_eq!(h.state().0.last_marker_spans(), vec![8..11]);
    click_pos(&mut h, p(0, 9), Modifiers::NONE);
    type_text(&mut h, "y");
    assert!(h.state().0.markers().is_empty());
}

#[test]
fn completion_popup_accepts_with_enter() {
    let mut h = harness("");
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    type_text(&mut h, "SEL");
    assert!(h.state().0.completion_open());
    assert_eq!(h.state().0.completion_labels()[0], "SELECT");
    key(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(text(&h), "SELECT");
    assert!(!h.state().0.completion_open());
}

#[test]
fn completion_closes_on_other_chars_and_caret_moves() {
    let mut h = harness("");
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    type_text(&mut h, "s");
    assert!(!h.state().0.completion_open(), "one char in General");
    type_text(&mut h, "e");
    assert!(h.state().0.completion_open());
    type_text(&mut h, " ");
    assert!(!h.state().0.completion_open());
    type_text(&mut h, "fr");
    assert!(h.state().0.completion_open());
    key(&mut h, Modifiers::NONE, Key::ArrowLeft);
    assert!(!h.state().0.completion_open());
    key(&mut h, Modifiers::CTRL, Key::Space);
    assert!(h.state().0.completion_open());
    key(&mut h, Modifiers::NONE, Key::Escape);
    assert!(!h.state().0.completion_open());
    assert!(h.state().0.has_focus());
}

#[test]
fn popup_updates_when_catalog_arrives() {
    let source = StubSource {
        available: true,
        schemas: Some(vec!["public".into()]),
        defaults: vec!["public".into()],
        ..StubSource::default()
    };
    let mut h = harness_with("", source);
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    type_text(&mut h, "select * from public.");
    assert!(h.state().0.completion_open());
    assert!(
        h.state()
            .1
            .requested
            .contains(&CatalogNeed::Schema("public".into())),
        "{:?}",
        h.state().1.requested
    );
    h.get_by_label("Loading…");
    h.run_steps(3);
    let public = CatalogNeed::Schema("public".into());
    let count = h
        .state()
        .1
        .requested
        .iter()
        .filter(|n| **n == public)
        .count();
    assert_eq!(count, 1, "each need once per popup session");

    h.state_mut().1.catalogs.insert(
        "public".into(),
        SchemaCatalog {
            schema: "public".into(),
            relations: vec![CatalogRelation {
                name: "orders".into(),
                kind: RelationKind::Table,
                columns: Vec::new(),
            }],
        },
    );
    h.run_steps(2);
    assert!(
        h.state()
            .0
            .completion_labels()
            .contains(&"orders".to_string()),
        "{:?}",
        h.state().0.completion_labels()
    );
    key(&mut h, Modifiers::NONE, Key::Tab);
    assert_eq!(text(&h), "select * from public.orders");
}

#[test]
fn run_target_prefers_selection() {
    let mut s = EditorState::new("select 1;\nselect 2", SqlSyntax::ansi());
    s.set_carets(vec![Caret {
        anchor: p(1, 0),
        head: p(1, 8),
    }]);
    assert_eq!(
        s.run_target(),
        Some(RunRequest {
            sql: "select 2".into(),
            origin: 10
        })
    );
    s.set_carets(vec![Caret::at(p(0, 3))]);
    assert_eq!(
        s.run_target(),
        Some(RunRequest {
            sql: "select 1".into(),
            origin: 0
        })
    );
    s.set_carets(vec![Caret {
        anchor: p(0, 0),
        head: p(0, 2),
    }]);
    assert_eq!(s.run_target().map(|r| r.sql), Some("se".to_string()));
}

#[test]
fn script_targets_split_whole_text() {
    let mut s = EditorState::new("select 1;\n\nselect 'a;b';\n select 3", SqlSyntax::ansi());
    assert_eq!(
        s.script_targets(),
        [
            RunRequest {
                sql: "select 1".into(),
                origin: 0
            },
            RunRequest {
                sql: "select 'a;b'".into(),
                origin: 11
            },
            RunRequest {
                sql: "select 3".into(),
                origin: 26
            },
        ]
    );
    s.set_carets(vec![Caret {
        anchor: p(2, 0),
        head: p(3, 9),
    }]);
    assert_eq!(s.script_targets().len(), 2);
}

#[test]
fn insert_text_is_one_undo_group() {
    let mut h = harness("a\nb");
    click_pos(&mut h, p(0, 1), Modifiers::NONE);
    click_pos(&mut h, p(1, 1), Modifiers::ALT);
    h.state_mut().0.insert_text("XY");
    h.run_steps(2);
    assert_eq!(text(&h), "aXY\nbXY");
    key(&mut h, Modifiers::COMMAND, Key::Z);
    assert_eq!(text(&h), "a\nb");
}

#[test]
fn clicking_a_completion_accepts_it() {
    let mut h = harness("x\n\n\n");
    click_pos(&mut h, p(0, 1), Modifiers::NONE);
    type_text(&mut h, " coal");
    assert_eq!(h.state().0.completion_labels(), ["coalesce"]);
    h.get_by_label_contains("coalesce").click();
    h.run_steps(2);
    assert_eq!(text(&h), "x coalesce()\n\n\n");
    assert_eq!(
        primary(&h),
        Caret::at(p(0, 11)),
        "caret inside the parentheses"
    );
    assert!(h.state().0.has_focus());
}

#[test]
fn dot_after_number_does_not_open_popup() {
    let mut h = harness("");
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    for t in ["select 1", ".", "5"] {
        type_text(&mut h, t);
        assert!(!h.state().0.completion_open(), "after {t:?}");
    }
    key(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(text(&h), "select 1.5\n");
}

#[test]
fn backspacing_the_prefix_closes_an_implicit_popup() {
    let mut h = harness("");
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    type_text(&mut h, "x se");
    assert!(h.state().0.completion_open());
    key(&mut h, Modifiers::NONE, Key::Backspace);
    assert!(h.state().0.completion_open(), "one char of prefix left");
    key(&mut h, Modifiers::NONE, Key::Backspace);
    assert!(!h.state().0.completion_open());
    key(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(text(&h), "x \n");

    // A popup opened with Ctrl+Space survives an empty prefix.
    key(&mut h, Modifiers::CTRL, Key::Space);
    assert!(h.state().0.completion_open());
    type_text(&mut h, "s");
    key(&mut h, Modifiers::NONE, Key::Backspace);
    assert!(h.state().0.completion_open());
}

#[test]
fn ctrl_space_toggles_completion() {
    let mut h = harness("");
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    key(&mut h, Modifiers::CTRL, Key::Space);
    assert!(h.state().0.completion_open(), "the first press opens it");
    key(&mut h, Modifiers::CTRL, Key::Space);
    assert!(!h.state().0.completion_open(), "the second press closes it");
    key(&mut h, Modifiers::CTRL, Key::Space);
    assert!(h.state().0.completion_open(), "the third press reopens it");
    key(&mut h, Modifiers::NONE, Key::Escape);
    assert!(!h.state().0.completion_open());
}

#[test]
fn double_click_selects_word_and_triple_click_line() {
    let mut h = harness("select foo_bar, x\nnext");
    for _ in 0..2 {
        click_pos(&mut h, p(0, 9), Modifiers::NONE);
    }
    assert_eq!(h.state().0.model().selected_text(), "foo_bar");
    click_pos(&mut h, p(0, 9), Modifiers::NONE);
    assert_eq!(h.state().0.model().selected_text(), "select foo_bar, x\n");
    // A later single click collapses the selection again.
    std::thread::sleep(std::time::Duration::from_millis(5));
    h.run_steps(40);
    click_pos(&mut h, p(1, 2), Modifiers::NONE);
    assert_eq!(primary(&h), Caret::at(p(1, 2)));
}

#[test]
fn reveal_caret_scrolls_a_far_caret_into_view() {
    let text = "select 1;\n".repeat(5000);
    let mut h = harness(&text);
    h.state_mut().0.set_carets(vec![Caret::at(p(4000, 3))]);
    h.state_mut().0.request_focus();
    h.state_mut().0.reveal_caret();
    h.run_steps(3);
    assert!(h.state().0.screen_pos(p(4000, 3)).is_some());
}

#[test]
fn query_editor_has_the_text_menu() {
    let mut h = harness("select 1");
    h.get_by_label("SQL editor").click_secondary();
    h.run_steps(2);
    for item in ["Undo", "Redo", "Cut", "Copy", "Paste", "Select all"] {
        h.get_by_label(item);
    }
}

#[test]
fn copy_from_the_query_editor_menu() {
    let mut h = harness("abc def");
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    h.state_mut().0.set_carets(vec![Caret {
        anchor: p(0, 0),
        head: p(0, 3),
    }]);
    h.run_steps(2);
    h.get_by_label("SQL editor").click_secondary();
    h.run_steps(2);
    h.get_by_label("Copy").click();
    let mut copied = false;
    for _ in 0..3 {
        h.step();
        copied |= h
            .output()
            .platform_output
            .commands
            .contains(&OutputCommand::CopyText("abc".into()));
    }
    assert!(copied, "{:?}", h.output().platform_output.commands);
}

#[test]
fn json_editor_has_the_text_menu() {
    let mut state = EditorState::new("{\"a\": 1}", SqlSyntax::ansi());
    state.set_language(QueryLanguage::Json);
    let mut h = Harness::builder()
        .with_size(vec2(900.0, 600.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, s: &mut State| {
                let out = query_editor(ui, &mut s.0, &s.1);
                s.1.requested.extend(out.needs);
            },
            (state, StubSource::default()),
        );
    h.run_steps(2);
    h.get_by_label("JSON editor").click_secondary();
    h.run_steps(2);
    for item in ["Undo", "Redo", "Cut", "Copy", "Paste", "Select all"] {
        h.get_by_label(item);
    }
}

#[test]
fn statement_tint_covers_caret_statement() {
    let mut h = harness("select 1;\nselect 2;\n");
    h.state_mut().0.set_carets(vec![Caret::at(p(1, 3))]);
    h.run_steps(2);
    assert_eq!(h.state().0.tint_lines(), Some(1..2));
    h.state_mut().0.set_carets(vec![Caret::at(p(0, 3))]);
    h.run_steps(2);
    assert_eq!(h.state().0.tint_lines(), Some(0..1));
    // Turned off, nothing is tinted.
    h.state_mut().0.set_statement_tint(false);
    h.run_steps(2);
    assert_eq!(h.state().0.tint_lines(), None);
}

#[test]
fn soft_wrap_wraps_at_width() {
    let text = "x".repeat(300);
    let mut h = harness_sized(&text, vec2(400.0, 600.0));
    // Off, the line is one visual row.
    assert_eq!(h.state().0.wrapped_rows(0), Some(1));
    h.state_mut().0.set_soft_wrap(true);
    h.run_steps(2);
    assert!(
        h.state().0.wrapped_rows(0).is_some_and(|n| n >= 3),
        "{:?}",
        h.state().0.wrapped_rows(0)
    );
    // Down moves within the wrapped line, not past it.
    h.state_mut().0.set_carets(vec![Caret::at(p(0, 0))]);
    h.state_mut().0.request_focus();
    key(&mut h, Modifiers::NONE, Key::ArrowDown);
    let head = primary(&h).head;
    assert_eq!(head.line, 0, "{head:?}");
    assert!(head.col > 0, "{head:?}");
}

#[test]
fn wrap_hit_testing_multibyte() {
    // 41 chars: CJK (double width) with an emoji in the middle, which no monospace
    // column arithmetic can place.
    let text = format!(
        "{}{}{}",
        "数据表查询".repeat(4),
        "😀",
        "数据表查询".repeat(4)
    );
    let mut h = harness_sized(&text, vec2(260.0, 600.0));
    h.state_mut().0.set_soft_wrap(true);
    h.run_steps(2);
    let start = h.state().0.screen_pos(p(0, 0)).unwrap();
    let target = p(0, 30);
    let at = h.state().0.screen_pos(target).unwrap();
    assert!(at.y > start.y + 1.0, "col 30 is on a later visual row");
    click_at(&mut h, at + vec2(2.0, 0.0), Modifiers::NONE);
    assert_eq!(primary(&h).head, target);
}

#[test]
fn wrap_on_from_the_start_keeps_a_long_line_reachable() {
    // Wrap is on before the first frame: the long line never gets a stale `max_width`
    // from an unwrapped frame, yet its far end must scroll into view.
    let text = format!("select '{}'", "x".repeat(9_990));
    let mut h = harness_wrapped(&text, vec2(400.0, 300.0));
    h.state_mut().0.set_carets(vec![Caret::at(p(0, 9_991))]);
    h.state_mut().0.request_focus();
    h.state_mut().0.reveal_caret();
    h.run_steps(4);
    let at = h
        .state()
        .0
        .screen_pos(p(0, 9_991))
        .expect("caret on screen");
    assert!(at.x < 400.0, "the caret scrolled into view, x = {}", at.x);
}

#[test]
fn wrapped_rows_sit_on_whole_pixels() {
    // Galley rows are stacked at pixel-rounded heights; the editor's row geometry must use
    // the same height or hit-testing drifts one row per few dozen rows.
    let text = "abcdefghij ".repeat(300);
    let mut h = harness_wrapped(&text, vec2(260.0, 600.0));
    h.ctx.set_pixels_per_point(1.3);
    h.run_steps(3);
    let y0 = h.state().0.screen_pos(p(0, 0)).unwrap().y;
    let y1 = h.state().0.screen_pos(p(0, 60)).unwrap().y;
    let ppp = h.ctx.pixels_per_point();
    let step = (y1 - y0) * ppp;
    assert!(
        (step - step.round()).abs() < 1e-3,
        "row step {step} device pixels is not whole"
    );
}

#[test]
fn rebuilt_wrap_index_counts_a_long_line_as_one_row() {
    // The long line is far below the viewport, so no galley corrects its count.
    let text = format!("{}{}", "select 1;\n".repeat(500), "x".repeat(5_000));
    let h = harness_wrapped(&text, vec2(260.0, 300.0));
    assert_eq!(h.state().0.wrapped_rows(500), Some(1));
}

#[test]
fn line_numbers_toggle_hides_gutter() {
    let mut h = harness("select 1\nselect 2");
    let with = screen(&h, p(0, 0)).x;
    h.state_mut().0.set_line_numbers(false);
    h.run_steps(2);
    let without = screen(&h, p(0, 0)).x;
    assert!(with - without >= 20.0, "gutter: {with} vs {without}");
}

#[test]
fn autocomplete_options_respected() {
    let mut h = harness_with("", StubSource::default());
    h.state_mut()
        .0
        .set_completion_options(sqlr_ui::CompletionOptions {
            auto_open: false,
            ..Default::default()
        });
    h.state_mut().0.request_focus();
    h.run_steps(2);
    type_text(&mut h, "SEL");
    assert!(!h.state().0.completion_open());
    key(&mut h, Modifiers::CTRL, Key::Space);
    assert!(h.state().0.completion_open());
    h.state_mut()
        .0
        .set_completion_options(sqlr_ui::CompletionOptions {
            auto_open: false,
            insert_on_tab: false,
            keyword_case: sqlr_core::Case::Lower,
            ..Default::default()
        });
    key(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().0.text(), "select");
}

fn sel(anchor: Pos, head: Pos) -> Caret {
    Caret { anchor, head }
}

fn carets(h: &H) -> Vec<Caret> {
    h.state().0.model().carets().to_vec()
}

#[test]
fn cmd_d_adds_next_occurrences_and_cmd_u_takes_one_back() {
    let mut h = harness("id x id\nID id");
    click_pos(&mut h, p(0, 1), Modifiers::NONE);
    key(&mut h, Modifiers::COMMAND, Key::D);
    assert_eq!(carets(&h), [sel(p(0, 0), p(0, 2))]);
    key(&mut h, Modifiers::COMMAND, Key::D);
    key(&mut h, Modifiers::COMMAND, Key::D);
    assert_eq!(
        carets(&h),
        [
            sel(p(0, 0), p(0, 2)),
            sel(p(0, 5), p(0, 7)),
            sel(p(1, 3), p(1, 5)),
        ]
    );
    key(&mut h, Modifiers::COMMAND, Key::U);
    assert_eq!(carets(&h).len(), 2);
    type_text(&mut h, "k");
    assert_eq!(text(&h), "k x k\nID id");
    key(&mut h, Modifiers::COMMAND, Key::Z);
    assert_eq!(text(&h), "id x id\nID id");
}

#[test]
fn cmd_shift_l_selects_every_occurrence() {
    let mut h = harness("id x id\nID id");
    click_pos(&mut h, p(1, 4), Modifiers::NONE);
    key(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::L);
    assert_eq!(carets(&h).len(), 3);
    assert_eq!(primary(&h), sel(p(1, 3), p(1, 5)));
    type_text(&mut h, "n");
    assert_eq!(text(&h), "n x n\nID n");
}

#[test]
fn alt_arrows_move_lines_and_shift_cmd_k_deletes_them() {
    let mut h = harness("a\nb\nc");
    click_pos(&mut h, p(0, 1), Modifiers::NONE);
    key(&mut h, Modifiers::ALT, Key::ArrowDown);
    assert_eq!(text(&h), "b\na\nc");
    assert_eq!(primary(&h), Caret::at(p(1, 1)));
    key(&mut h, Modifiers::ALT | Modifiers::SHIFT, Key::ArrowDown);
    assert_eq!(text(&h), "b\na\na\nc");
    assert_eq!(primary(&h), Caret::at(p(2, 1)));
    key(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::K);
    assert_eq!(text(&h), "b\na\nc");
    key(&mut h, Modifiers::COMMAND, Key::Z);
    assert_eq!(text(&h), "b\na\na\nc");
}

#[test]
fn a_rebound_chord_runs_the_editor_command() {
    let mut h = harness("a\nb");
    let mut keymap = sqlr_core::Keymap::preset("sqlr").unwrap();
    keymap.set_chords(
        sqlr_core::ActionId::DeleteLine,
        vec![sqlr_core::Chord::parse("Cmd+E").unwrap()],
    );
    sqlr_ui::keymap::set_ui_keymap(&h.ctx, keymap);
    click_pos(&mut h, p(0, 0), Modifiers::NONE);
    key(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::K);
    assert_eq!(text(&h), "a\nb", "the old chord is unbound");
    key(&mut h, Modifiers::COMMAND, Key::E);
    assert_eq!(text(&h), "b");
}

#[test]
fn an_alt_chord_does_not_type_its_character() {
    // On macOS ⇧⌥I also sends the text "ˆ"; the command must not type it.
    let mut h = harness("ab\ncd");
    click_pos(&mut h, p(0, 1), Modifiers::NONE);
    key(&mut h, Modifiers::SHIFT, Key::ArrowDown);
    h.event(Event::Key {
        key: Key::I,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::ALT | Modifiers::SHIFT,
    });
    h.event(Event::Text("ˆ".into()));
    h.run_steps(2);
    assert_eq!(text(&h), "ab\ncd");
    assert_eq!(carets(&h), [Caret::at(p(0, 2)), Caret::at(p(1, 2))]);
    type_text(&mut h, ";");
    assert_eq!(text(&h), "ab;\ncd;");
}
