//! The shared dialog: header, body, footer and keyboard behaviour.

use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sqlr_ui::theme::{self, Dialog, Palette, icons};

#[derive(Default)]
struct State {
    /// The reason the primary is disabled, if it is.
    blocked: Option<&'static str>,
    danger: bool,
    rows: usize,
    primary: usize,
    closed: usize,
    secondary: usize,
}

fn harness(size: (f32, f32), state: State) -> Harness<'static, State> {
    Harness::builder().with_size(size).build_ui_state(
        |ui, s: &mut State| {
            theme::apply(ui.ctx(), &Palette::dark(), 13.0);
            let mut dialog = Dialog::new("test", icons::EXPORT, "Export orders")
                .subtitle("Choose the format of the file")
                .width(460.0);
            if s.danger {
                dialog = dialog.danger();
            }
            let response = dialog.show(ui.ctx(), |d| {
                d.body(|ui| {
                    ui.label("Body text");
                    for i in 0..s.rows {
                        ui.label(format!("Row {i}"));
                    }
                });
                d.footer(|f| {
                    if f.secondary("Save as task", true) {
                        s.secondary += 1;
                    }
                    let clicked = if s.danger {
                        f.danger("Delete", s.blocked)
                    } else {
                        f.primary("Export", s.blocked)
                    };
                    if clicked {
                        s.primary += 1;
                    }
                    f.cancel();
                });
            });
            if response.closed {
                s.closed += 1;
            }
        },
        state,
    )
}

#[test]
fn the_header_names_the_dialog_and_its_close_button_closes_it() {
    let mut h = harness((900.0, 700.0), State::default());
    h.run_steps(3);
    h.get_by_label("Export orders");
    h.get_by_label("Choose the format of the file");
    h.get_by_label("Export orders dialog");
    h.get_by_label("Close Export orders").click();
    h.run();
    assert_eq!(h.state().closed, 1);
    assert_eq!(h.state().primary, 0);
}

#[test]
fn escape_and_cancel_close_the_dialog() {
    let mut h = harness((900.0, 700.0), State::default());
    h.run_steps(3);
    h.key_press(Key::Escape);
    h.run();
    assert_eq!(h.state().closed, 1);
    h.get_by_label("Cancel").click();
    h.run();
    assert_eq!(h.state().closed, 2);
    assert_eq!(h.state().primary, 0);
}

#[test]
fn the_footer_puts_secondary_actions_left_and_cancel_before_the_primary() {
    let mut h = harness((900.0, 700.0), State::default());
    h.run_steps(3);
    let secondary = h.get_by_label("Save as task").rect();
    let cancel = h.get_by_label("Cancel").rect();
    let primary = h.get_by_label("Export").rect();
    assert!(
        secondary.right() < cancel.left(),
        "{secondary:?} {cancel:?}"
    );
    assert!(cancel.right() < primary.left(), "{cancel:?} {primary:?}");
    assert!(
        (cancel.center().y - primary.center().y).abs() < 1.0
            && (secondary.center().y - primary.center().y).abs() < 1.0
    );
    h.get_by_label("Save as task").click();
    h.run();
    assert_eq!(h.state().secondary, 1);
    assert_eq!(h.state().closed, 0);
}

#[test]
fn enter_presses_the_enabled_primary() {
    let mut h = harness((900.0, 700.0), State::default());
    h.run_steps(3);
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().primary, 1);
    h.get_by_label("Export").click();
    h.run();
    assert_eq!(h.state().primary, 2);
}

#[test]
fn a_disabled_primary_says_why() {
    let state = State {
        blocked: Some("Choose a file to import"),
        ..State::default()
    };
    let mut h = harness((900.0, 700.0), state);
    h.run_steps(3);
    assert!(h.get_by_label("Export").accesskit_node().is_disabled());
    let reason = h.get_by_label("Choose a file to import").rect();
    assert!(reason.right() < h.get_by_label("Cancel").rect().left());
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(
        h.state().primary,
        0,
        "Enter must not press a disabled primary"
    );
}

#[test]
fn a_destructive_dialog_focuses_cancel_and_ignores_enter() {
    let state = State {
        danger: true,
        ..State::default()
    };
    let mut h = harness((900.0, 700.0), state);
    h.run_steps(3);
    assert!(h.get_by_label("Cancel").accesskit_node().is_focused());
    h.get_by_label("Delete");
    // Enter presses the focused Cancel, never Delete.
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().primary, 0);
    assert_eq!(h.state().closed, 1);
    h.get_by_label("Delete").click();
    h.run();
    assert_eq!(h.state().primary, 1);
}

#[test]
fn a_long_body_scrolls_and_keeps_the_footer_on_a_small_window() {
    let state = State {
        rows: 80,
        ..State::default()
    };
    let mut h = harness((520.0, 360.0), state);
    h.run_steps(6);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(520.0, 360.0));
    let dialog = h.get_by_label("Export orders dialog").rect();
    assert!(
        screen.contains_rect(dialog),
        "dialog {dialog:?} leaves the window"
    );
    for action in ["Cancel", "Export", "Close Export orders"] {
        let rect = h.get_by_label(action).rect();
        assert!(
            dialog.contains_rect(rect),
            "{action} {rect:?} outside {dialog:?}"
        );
    }
    // The last rows are scrolled away, not drawn past the footer.
    let last = h.query_by_label("Row 79");
    assert!(
        last.is_none_or(|n| n.rect().top() >= dialog.bottom()
            || !dialog.contains_rect(n.rect())
            || n.rect().bottom() <= h.get_by_label("Cancel").rect().top()),
        "Row 79 overlaps the footer"
    );
}

#[test]
fn a_short_dialog_fits_its_content() {
    let mut h = harness((900.0, 900.0), State::default());
    h.run_steps(4);
    let dialog = h.get_by_label("Export orders dialog").rect();
    assert!(
        dialog.height() < 220.0,
        "a one-line body should not leave empty space: {dialog:?}"
    );
    // Centered on the window.
    assert!((dialog.center().x - 450.0).abs() < 2.0, "{dialog:?}");
    assert!((dialog.center().y - 450.0).abs() < 2.0, "{dialog:?}");
}
