//! The key-value widgets: key browser, value view and console.

use std::sync::Arc;

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sqlr_core::{KeyInfo, KeyType, KvEdit, KvPage, RespValue};
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{
    ConsoleState, KeyBrowserAction, KeyBrowserState, KvViewAction, KvViewState, console,
    key_browser, key_value_view,
};

fn theme_apply(ctx: &egui::Context) {
    theme::apply(ctx, &Palette::dark(), 13.0);
}

// ----- key browser -----

struct Browser {
    state: KeyBrowserState,
    keys: Vec<KeyInfo>,
    more: bool,
    actions: Vec<KeyBrowserAction>,
}

fn key(name: &str, ty: KeyType) -> KeyInfo {
    KeyInfo::new(name, ty)
}

fn browser(keys: Vec<KeyInfo>, more: bool) -> Harness<'static, Browser> {
    let app = Browser {
        state: KeyBrowserState::default(),
        keys,
        more,
        actions: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(420.0, 520.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut Browser| {
                theme_apply(ui.ctx());
                if let Some(a) = key_browser(ui, &mut app.state, &app.keys, app.more, ":") {
                    app.actions.push(a);
                }
            },
            app,
        );
    h.run_steps(2);
    h
}

fn sample() -> Vec<KeyInfo> {
    vec![
        key("user:1", KeyType::Hash),
        key("user:2", KeyType::Hash),
        key("order:7", KeyType::List),
        key("plain", KeyType::String),
    ]
}

#[test]
fn key_browser_groups_by_separator() {
    let mut h = browser(sample(), false);
    // Groups first, then the keys without a separator; leaves stay hidden until expanded.
    h.get_by_label("Expand user");
    h.get_by_label("Expand order");
    h.get_by_label("plain");
    assert!(h.query_by_label("1").is_none());
    h.get_by_label("Expand user").click();
    h.run_steps(2);
    h.get_by_label("Collapse user");
    h.get_by_label("1");
    h.get_by_label("2");
    assert!(h.query_by_label("7").is_none(), "order stays collapsed");

    h.get_by_label("1").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        vec![KeyBrowserAction::Open(b"user:1".to_vec())]
    );
    h.get_by_label("plain").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions.last(),
        Some(&KeyBrowserAction::Open(b"plain".to_vec()))
    );
    // A group can be collapsed again.
    h.get_by_label("Collapse user").click();
    h.run_steps(2);
    assert!(h.query_by_label("2").is_none());
}

#[test]
fn key_browser_pattern_load_more_and_refresh() {
    let mut h = browser(sample(), true);
    h.get_by_role_and_label(Role::TextInput, "Key pattern")
        .focus();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Key pattern")
        .type_text("user:*");
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        vec![KeyBrowserAction::SetPattern("user:*".into())]
    );
    h.get_by_label("Load more").click();
    h.run_steps(2);
    h.get_by_label("Refresh").click();
    h.run_steps(2);
    h.get_by_label("New key").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions[1..],
        [
            KeyBrowserAction::LoadMore,
            KeyBrowserAction::Refresh,
            KeyBrowserAction::NewKey
        ]
    );
}

#[test]
fn key_browser_without_more_has_no_load_more() {
    let h = browser(sample(), false);
    assert!(h.query_by_label("Load more").is_none());
}

#[test]
fn key_browser_multi_select_delete_asks_first() {
    let mut h = browser(sample(), false);
    h.get_by_label("Expand user").click();
    h.run_steps(2);
    h.get_by_label("1").click();
    h.run_steps(2);
    h.get_by_label("2").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    h.state_mut().actions.clear();
    h.get_by_label("Delete 2 keys").click();
    h.run_steps(2);
    // Nothing is deleted until the confirmation says so.
    assert!(h.state().actions.is_empty());
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().actions.is_empty());
    h.get_by_label("Delete 2 keys").click();
    h.run_steps(2);
    h.get_by_label("Delete").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        vec![KeyBrowserAction::Delete(vec![
            b"user:1".to_vec(),
            b"user:2".to_vec()
        ])]
    );
}

#[test]
fn key_browser_shift_click_selects_a_range() {
    let keys = ["a", "b", "c", "d"]
        .map(|n| key(n, KeyType::String))
        .to_vec();
    let mut h = browser(keys, false);
    h.get_by_label("a").click();
    h.run_steps(2);
    h.get_by_label("c").click_modifiers(Modifiers::SHIFT);
    h.run_steps(2);
    h.get_by_label("Delete 3 keys");
}

#[test]
fn binary_key_names_show_escaped() {
    let keys = vec![KeyInfo::new(b"bin:\xff\x00".to_vec(), KeyType::String)];
    let mut h = browser(keys, false);
    h.get_by_label("Expand bin").click();
    h.run_steps(2);
    h.get_by_label("\\xff\\x00");
}

// ----- value view -----

struct View {
    state: KvViewState,
    key: KeyInfo,
    page: Option<KvPage>,
    actions: Vec<KvViewAction>,
}

fn view(key: KeyInfo, page: Option<KvPage>) -> Harness<'static, View> {
    let app = View {
        state: KvViewState::default(),
        key,
        page,
        actions: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(640.0, 520.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut View| {
                theme_apply(ui.ctx());
                if let Some(a) = key_value_view(ui, &mut app.state, &app.key, app.page.as_ref()) {
                    app.actions.push(a);
                }
            },
            app,
        );
    h.run_steps(2);
    h
}

fn hash_view() -> Harness<'static, View> {
    view(
        key("h", KeyType::Hash),
        Some(KvPage::Hash {
            fields: vec![
                (b"a".to_vec(), b"1".to_vec()),
                (b"b".to_vec(), b"2".to_vec()),
            ],
            cursor: 0,
        }),
    )
}

#[test]
fn value_view_hash_edit_emits_action() {
    let mut h = hash_view();
    h.get_by_label("a");
    h.get_by_label("1");
    h.get_by_label("Edit a").click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(Event::Text("22".into()));
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        vec![KvViewAction::Edit(KvEdit::HashSet(
            b"a".to_vec(),
            b"22".to_vec()
        ))]
    );
}

#[test]
fn value_view_hash_delete_field_and_add_field() {
    let mut h = hash_view();
    h.get_by_label("Remove b").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        vec![KvViewAction::Edit(KvEdit::HashDel(b"b".to_vec()))]
    );
    h.state_mut().actions.clear();
    h.get_by_role_and_label(Role::TextInput, "New field")
        .focus();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "New field")
        .type_text("c");
    h.get_by_role_and_label(Role::TextInput, "New value")
        .focus();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "New value")
        .type_text("3");
    h.run_steps(2);
    h.get_by_label("Add").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        vec![KvViewAction::Edit(KvEdit::HashSet(
            b"c".to_vec(),
            b"3".to_vec()
        ))]
    );
}

#[test]
fn value_view_ttl_persist_rename_and_delete() {
    let info = KeyInfo {
        ttl_ms: Some(90_000),
        ..key("k", KeyType::String)
    };
    let mut h = view(
        info,
        Some(KvPage::String {
            bytes: b"hello".to_vec(),
            total_len: 5,
        }),
    );
    h.get_by_label("Persist").click();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "TTL").focus();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(Event::Text("5m".into()));
    h.run_steps(2);
    h.get_by_label("Set TTL").click();
    h.run_steps(2);
    h.get_by_label("Rename").click();
    h.run_steps(2);
    // The field starts with the current name.
    h.get_by_role_and_label(Role::TextInput, "New name").focus();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(Event::Text("k2".into()));
    h.run_steps(2);
    h.get_by_label("Rename to").click();
    h.run_steps(2);
    h.get_by_label("Delete key").click();
    h.run_steps(2);
    // The confirmation comes first.
    assert_eq!(h.state().actions.len(), 3);
    h.get_by_label("Delete").click();
    h.run_steps(2);
    h.get_by_label("Refresh").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        vec![
            KvViewAction::Edit(KvEdit::Expire(None)),
            KvViewAction::Edit(KvEdit::Expire(Some(300_000))),
            KvViewAction::Edit(KvEdit::Rename(b"k2".to_vec())),
            KvViewAction::Edit(KvEdit::Delete),
            KvViewAction::Refresh,
        ]
    );
}

#[test]
fn value_view_string_edit_saves_text() {
    let mut h = view(
        key("k", KeyType::String),
        Some(KvPage::String {
            bytes: b"hello".to_vec(),
            total_len: 5,
        }),
    );
    h.get_by_role_and_label(Role::TextInput, "String value")
        .focus();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(Event::Text("bye".into()));
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        vec![KvViewAction::Edit(KvEdit::SetString(b"bye".to_vec()))]
    );
}

#[test]
fn value_view_truncated_strings_load_more_and_are_not_editable() {
    let mut h = view(
        key("big", KeyType::String),
        Some(KvPage::String {
            bytes: vec![b'x'; 1000],
            total_len: 5_000_000,
        }),
    );
    // Saving a preview would cut the value: no editor, no Save.
    assert!(h.query_by_label("Save").is_none());
    assert!(
        h.query_by_role_and_label(Role::TextInput, "String value")
            .is_none()
    );
    h.get_by_label("Load more").click();
    h.run_steps(2);
    assert_eq!(h.state().actions, vec![KvViewAction::LoadMore]);
}

#[test]
fn value_view_binary_strings_show_hex() {
    let h = view(
        key("bin", KeyType::String),
        Some(KvPage::String {
            bytes: vec![0xff, 0x00, 0x41],
            total_len: 3,
        }),
    );
    h.get_by_label_contains("ff 00 41");
}

#[test]
fn value_view_collections_list_their_elements() {
    let h = view(
        key("l", KeyType::List),
        Some(KvPage::List {
            items: vec![b"x".to_vec(), b"y".to_vec()],
            start: 0,
            total: 2,
        }),
    );
    h.get_by_label("x");
    h.get_by_label("y");
    let h = view(
        key("z", KeyType::ZSet),
        Some(KvPage::ZSet {
            members: vec![(b"m".to_vec(), 1.5)],
            start: 0,
            total: 1,
        }),
    );
    h.get_by_label("m");
    h.get_by_label("1.5");
    let h = view(
        key("s", KeyType::Stream),
        Some(KvPage::Stream {
            entries: vec![("1-0".into(), vec![(b"k".to_vec(), b"v".to_vec())])],
            next: None,
            total: 1,
        }),
    );
    h.get_by_label("1-0");
    h.get_by_label("k=v");
}

#[test]
fn value_view_module_types_say_use_the_console() {
    let h = view(key("j", KeyType::Json), None);
    h.get_by_label_contains("console");
}

#[test]
fn value_view_list_load_more() {
    let mut h = view(
        key("l", KeyType::List),
        Some(KvPage::List {
            items: vec![b"x".to_vec()],
            start: 0,
            total: 400,
        }),
    );
    h.get_by_label("Load more").click();
    h.run_steps(2);
    assert_eq!(h.state().actions, vec![KvViewAction::LoadMore]);
}

// ----- console -----

struct Console {
    state: ConsoleState,
    output: Vec<Arc<(String, RespValue)>>,
    lines: Vec<String>,
}

fn console_harness(
    history: &[&str],
    output: Vec<(String, RespValue)>,
) -> Harness<'static, Console> {
    let mut state = ConsoleState::default();
    state.history = Arc::new(history.iter().map(|h| h.to_string()).collect());
    let app = Console {
        state,
        output: output.into_iter().map(Arc::new).collect(),
        lines: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(600.0, 400.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut Console| {
                theme_apply(ui.ctx());
                if let Some(line) = console(ui, &mut app.state, &app.output) {
                    app.lines.push(line);
                }
            },
            app,
        );
    h.run_steps(3);
    h
}

#[test]
fn console_history_navigation() {
    let mut h = console_harness(&["GET a", "SET b 1"], Vec::new());
    h.key_press(Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(h.state().state.input, "SET b 1");
    h.key_press(Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(h.state().state.input, "GET a");
    h.key_press(Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(h.state().state.input, "GET a", "stops at the oldest");
    h.key_press(Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(h.state().state.input, "SET b 1");
    h.key_press(Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(h.state().state.input, "", "back to what was being typed");
}

#[test]
fn console_history_keeps_the_draft() {
    let mut h = console_harness(&["GET a"], Vec::new());
    h.get_by_role_and_label(Role::TextInput, "Console input")
        .type_text("DEL");
    h.run_steps(2);
    // DEL opens the completion popup, where the arrows move the selection: close it first.
    h.key_press(Key::Escape);
    h.run_steps(2);
    h.key_press(Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(h.state().state.input, "GET a");
    h.key_press(Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(h.state().state.input, "DEL");
}

#[test]
fn console_enter_returns_the_line_and_clears_the_input() {
    let mut h = console_harness(&[], Vec::new());
    h.get_by_role_and_label(Role::TextInput, "Console input")
        .type_text("PING");
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().lines, vec!["PING".to_string()]);
    assert_eq!(h.state().state.input, "");
    // An empty line returns nothing.
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().lines.len(), 1);
}

#[test]
fn console_tab_completes_the_command() {
    let mut h = console_harness(&[], Vec::new());
    h.get_by_role_and_label(Role::TextInput, "Console input")
        .type_text("get");
    h.run_steps(2);
    h.key_press(Key::Tab);
    h.run_steps(2);
    assert_eq!(h.state().state.input, "GET ");
}

#[test]
fn console_arrows_move_the_completion_selection() {
    let mut h = console_harness(&[], Vec::new());
    h.get_by_role_and_label(Role::TextInput, "Console input")
        .type_text("get");
    h.run_steps(2);
    h.key_press(Key::ArrowDown);
    h.run_steps(2);
    h.key_press(Key::Tab);
    h.run_steps(2);
    assert_eq!(h.state().state.input, "GETBIT ");
}

#[test]
fn console_click_completes_the_command() {
    let mut h = console_harness(&[], Vec::new());
    h.get_by_role_and_label(Role::TextInput, "Console input")
        .type_text("get");
    h.run_steps(2);
    h.get_by_label_contains("GETBIT").click();
    h.run_steps(2);
    assert_eq!(h.state().state.input, "GETBIT ");
}

#[test]
fn console_enter_completes_a_partial_command_then_runs_it() {
    let mut h = console_harness(&[], Vec::new());
    h.get_by_role_and_label(Role::TextInput, "Console input")
        .type_text("pin");
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().state.input, "PING ");
    assert!(h.state().lines.is_empty());
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().lines, vec!["PING".to_string()]);
}

#[test]
fn console_subcommands_complete_after_the_command() {
    let mut h = console_harness(&[], Vec::new());
    h.get_by_role_and_label(Role::TextInput, "Console input")
        .type_text("CONFIG g");
    h.run_steps(2);
    h.key_press(Key::Tab);
    h.run_steps(2);
    assert_eq!(h.state().state.input, "CONFIG GET ");
}

#[test]
fn console_ctrl_space_opens_subcommands_on_an_empty_prefix() {
    let mut h = console_harness(&[], Vec::new());
    h.get_by_role_and_label(Role::TextInput, "Console input")
        .type_text("CONFIG ");
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::CTRL, Key::Space);
    h.run_steps(2);
    h.key_press(Key::Tab);
    h.run_steps(2);
    assert_eq!(h.state().state.input, "CONFIG GET ");
}

#[test]
fn console_renders_replies_by_resp_type() {
    let output = vec![
        ("DBSIZE".to_string(), RespValue::Int(5)),
        ("GET missing".to_string(), RespValue::Nil),
        ("GET k".to_string(), RespValue::Bulk(b"v".to_vec())),
        (
            "BAD".to_string(),
            RespValue::Error("ERR unknown command".into()),
        ),
        ("SET k v".to_string(), RespValue::Simple("OK".into())),
        (
            "LRANGE l 0 -1".to_string(),
            RespValue::Array(vec![
                RespValue::Bulk(b"x".to_vec()),
                RespValue::Bulk(b"y".to_vec()),
            ]),
        ),
    ];
    let h = console_harness(&[], output);
    h.get_by_label("(integer) 5");
    h.get_by_label("(nil)");
    h.get_by_label("\"v\"");
    h.get_by_label("(error) ERR unknown command");
    h.get_by_label("OK");
    h.get_by_label("1) \"x\"");
    h.get_by_label("2) \"y\"");
    h.get_by_label_contains("DBSIZE");
}

#[test]
fn renaming_a_binary_key_keeps_its_bytes() {
    let info = key("placeholder", KeyType::String);
    let info = KeyInfo {
        name: b"a\xffb".to_vec(),
        ..info
    };
    let page = Some(KvPage::String {
        bytes: b"v".to_vec(),
        total_len: 1,
    });
    let mut h = view(info, page);
    h.get_by_label("Rename").click();
    h.run_steps(2);
    // Unchanged: the name is prefilled in its escaped form and no rename is emitted.
    h.get_by_label("Rename to").click();
    h.run_steps(2);
    assert!(h.state().actions.is_empty(), "{:?}", h.state().actions);
    // Changing the text after the binary part keeps the 0xff byte.
    h.get_by_label("Rename").click();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "New name").focus();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(Event::Text("\"a\\xffc\"".into()));
    h.run_steps(2);
    h.get_by_label("Rename to").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        vec![KvViewAction::Edit(KvEdit::Rename(b"a\xffc".to_vec()))]
    );
}

#[test]
fn list_remove_names_the_index_and_the_value() {
    let mut h = view(
        key("l", KeyType::List),
        Some(KvPage::List {
            items: vec![b"x".to_vec(), b"a".to_vec(), b"x".to_vec()],
            start: 0,
            total: 3,
        }),
    );
    h.get_by_label("Remove 2").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        vec![KvViewAction::Edit(KvEdit::ListRem {
            index: 2,
            value: b"x".to_vec()
        })]
    );
}

#[test]
fn selection_follows_the_key_list() {
    let mut h = browser(sample(), false);
    h.get_by_label("plain").click();
    h.run_steps(2);
    h.get_by_label("Delete 1 key");
    // The key is gone from the list (deleted elsewhere): nothing is selected any more.
    h.state_mut().keys.retain(|k| k.name != b"plain");
    h.run_steps(3);
    assert!(h.state().state.selected.is_empty());
    assert!(
        h.query_by_label("Delete 1 key")
            .is_some_and(|n| n.accesskit_node().is_disabled())
    );
}

#[test]
fn a_new_pattern_clears_the_selection() {
    let mut h = browser(sample(), false);
    h.get_by_label("plain").click();
    h.run_steps(2);
    assert_eq!(h.state().state.selected.len(), 1);
    h.get_by_role_and_label(Role::TextInput, "Key pattern")
        .focus();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Key pattern")
        .type_text("order:*");
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert!(h.state().state.selected.is_empty());
}
