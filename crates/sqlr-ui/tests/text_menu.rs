use egui::{Event, OutputCommand, TextEdit, WidgetInfo, WidgetType, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sqlr_ui::{text_edit, text_edit_read_only, text_edit_secret};

#[derive(Default)]
struct App {
    text: String,
}

fn harness(text: &str) -> Harness<'static, App> {
    let app = App {
        text: text.to_string(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(480.0, 300.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                let field = text_edit(
                    ui,
                    TextEdit::multiline(&mut app.text)
                        .desired_rows(3)
                        .desired_width(300.0),
                );
                field.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, "Field"));
            },
            app,
        );
    h.run_steps(2);
    h
}

fn open_menu(h: &mut Harness<'static, App>) {
    h.get_by_label("Field").click_secondary();
    h.run_steps(2);
}

#[test]
fn a_text_box_has_the_classic_menu() {
    let mut h = harness("hello");
    open_menu(&mut h);
    for item in ["Undo", "Redo", "Cut", "Copy", "Paste", "Select all"] {
        h.get_by_label(item);
    }
}

#[test]
fn select_all_and_typing_replace_the_text() {
    let mut h = harness("hello");
    open_menu(&mut h);
    h.get_by_label("Select all").click();
    h.run_steps(2);
    h.event(Event::Text("bye".into()));
    h.run_steps(2);
    assert_eq!(h.state().text, "bye");
}

#[test]
fn copy_from_the_menu_reaches_the_clipboard() {
    let mut h = harness("hello");
    open_menu(&mut h);
    h.get_by_label("Select all").click();
    h.run_steps(2);
    open_menu(&mut h);
    h.get_by_label("Copy").click();
    let mut copied = false;
    for _ in 0..5 {
        h.step();
        copied |= h
            .output()
            .platform_output
            .commands
            .contains(&OutputCommand::CopyText("hello".into()));
    }
    assert!(copied, "{:?}", h.output().platform_output.commands);
}

#[test]
fn a_secret_field_cannot_be_copied() {
    let app = App {
        text: "hunter2".into(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(480.0, 300.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                let field =
                    text_edit_secret(ui, TextEdit::singleline(&mut app.text).password(true));
                field.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, "Secret"));
            },
            app,
        );
    h.run_steps(2);
    h.get_by_label("Secret").click_secondary();
    h.run_steps(2);
    for item in ["Cut", "Copy"] {
        assert!(
            h.get_by_label(item).accesskit_node().is_disabled(),
            "{item} should be disabled on a password field"
        );
    }
    for item in ["Paste", "Select all"] {
        assert!(
            !h.get_by_label(item).accesskit_node().is_disabled(),
            "{item} should stay enabled on a password field"
        );
    }
}

#[test]
fn a_read_only_field_offers_copy_only() {
    let text = "read me";
    let mut h = Harness::builder()
        .with_size(vec2(480.0, 300.0))
        .with_step_dt(0.01)
        .build_ui_state(
            move |ui, _: &mut ()| {
                let mut shown = text;
                let field =
                    text_edit_read_only(ui, TextEdit::singleline(&mut shown).desired_width(200.0));
                field.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, "Read only"));
            },
            (),
        );
    h.run_steps(2);
    h.get_by_label("Read only").click_secondary();
    h.run_steps(2);
    for item in ["Copy", "Select all"] {
        assert!(
            !h.get_by_label(item).accesskit_node().is_disabled(),
            "{item} should be enabled"
        );
    }
    for item in ["Undo", "Redo", "Cut", "Paste"] {
        assert!(
            h.get_by_label(item).accesskit_node().is_disabled(),
            "{item} should be disabled"
        );
    }
}
