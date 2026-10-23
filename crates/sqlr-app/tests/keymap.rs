use sqlr_core::{ActionId, Chord, Keymap};
#[test]
fn remap_run_statement_works() {
    let mut map = Keymap::preset("sqlr").unwrap();
    map.set_chords(ActionId::RunStatement, vec![Chord::parse("Alt+R").unwrap()]);
    let ctx = egui::Context::default();
    let mut input = egui::RawInput::default();
    input.events.push(egui::Event::Key {
        key: egui::Key::R,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::ALT,
    });
    let mut actions = vec![];
    let mut output = ctx.run_ui(input, |ui| {
        actions = sqlr_app::dispatch_actions(ui.ctx(), &map, |_| true);
    });
    output.textures_delta.clear();
    assert_eq!(actions, vec![ActionId::RunStatement]);
    let mut old = egui::RawInput::default();
    old.events.push(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    let mut output = ctx.run_ui(old, |ui| {
        actions = sqlr_app::dispatch_actions(ui.ctx(), &map, |_| true);
    });
    output.textures_delta.clear();
    assert!(actions.is_empty());
}
#[test]
fn keymap_editor_detects_conflict() {
    let mut map = Keymap::preset("sqlr").unwrap();
    map.bind(ActionId::Commit, Chord::parse("Cmd+Shift+P").unwrap());
    let mut state = sqlr_ui::keymap_editor::KeymapEditor::default();
    let mut preset = "sqlr".to_string();
    let mut h = egui_kittest::Harness::new_ui(|ui| {
        state.show(ui, &mut map, &mut preset);
    });
    h.run_steps(2);
    use egui_kittest::kittest::Queryable;
    h.get_by_label_contains("Conflicting shortcuts");
}

#[test]
fn shifted_question_mark_has_one_logical_binding() {
    let mut map = Keymap::default();
    map.bind(ActionId::Commands, Chord::parse("Cmd+Shift+Slash").unwrap());
    map.bind(
        ActionId::Settings,
        Chord::parse("Cmd+Questionmark").unwrap(),
    );
    assert_eq!(map.conflicts(true).len(), 1);
    map.unbind(ActionId::Settings);
    let ctx = egui::Context::default();
    let mut input = egui::RawInput::default();
    input.events.push(egui::Event::Key {
        key: egui::Key::Questionmark,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
    });
    let mut actions = vec![];
    let mut output = ctx.run_ui(input, |ui| {
        actions = sqlr_app::dispatch_actions(ui.ctx(), &map, |_| true);
    });
    output.textures_delta.clear();
    assert_eq!(actions, vec![ActionId::Commands]);
}

#[test]
fn platform_copy_event_respects_rebinding() {
    let mut map = Keymap::preset("sqlr").unwrap();
    map.unbind(ActionId::Copy);
    map.set_chords(ActionId::RunStatement, vec![Chord::parse("Cmd+C").unwrap()]);
    let ctx = egui::Context::default();
    let mut input = egui::RawInput::default();
    input
        .events
        .push(egui::Event::ModifiersChanged(egui::Modifiers::COMMAND));
    input.events.push(egui::Event::Copy);
    let mut actions = vec![];
    let mut output = ctx.run_ui(input, |ui| {
        sqlr_ui::keymap::normalize_platform_clipboard(ui.ctx(), &map);
        actions = sqlr_app::dispatch_actions(ui.ctx(), &map, |_| true);
    });
    output.textures_delta.clear();
    assert_eq!(actions, vec![ActionId::RunStatement]);
}
