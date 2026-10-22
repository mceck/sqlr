//! Hovering a control changes its background, never its layout.

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sqlr_ui::theme::{self, Palette, icons};

fn harness() -> Harness<'static, ()> {
    let mut h = Harness::builder()
        .with_size(vec2(600.0, 300.0))
        .with_step_dt(0.01)
        .build_ui(|ui| {
            theme::apply(ui.ctx(), &Palette::dark(), 13.0);
            ui.horizontal(|ui| {
                // Frame arrives on hover.
                icons::button(ui, true, icons::RUN, None, "Run");
                // Frameless icon button.
                icons::icon_button(ui, icons::ADD, Some(Palette::dark().success), "Add");
                // Selectable labels drop their frame when unselected.
                let _ = ui.selectable_label(false, "unselected");
                let _ = ui.selectable_label(true, "selected");
                // Standard and small buttons.
                let _ = ui.button("plain");
                let _ = ui.small_button("small");
            });
        });
    h.run_steps(2);
    h
}

#[test]
fn hover_does_not_move_controls() {
    let mut h = harness();
    for label in ["Run", "Add", "unselected", "selected", "plain", "small"] {
        let before = h.get_by_label(label).rect();
        h.get_by_label(label).hover();
        h.step();
        let after = h.get_by_label(label).rect();
        assert_eq!(before, after, "{label} moved on hover");
        // Move the pointer away before measuring the next one.
        h.event(egui::Event::PointerMoved(egui::pos2(590.0, 290.0)));
        h.step();
    }
}
