use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sqlr_ui::theme::{self, Palette};
use sqlr_ui::{WizardAction, WizardStep, wizard_frame};

struct App {
    steps: Vec<WizardStep>,
    current: usize,
    actions: Vec<WizardAction>,
}

fn harness(valid: [bool; 3], current: usize) -> Harness<'static, App> {
    let steps = ["Source", "Target", "Review"]
        .iter()
        .zip(valid)
        .map(|(title, valid)| WizardStep {
            title: (*title).into(),
            valid,
        })
        .collect();
    let mut h = Harness::builder()
        .with_size(vec2(700.0, 400.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                let current = app.current;
                let action = wizard_frame(ui, &app.steps, current, |ui| {
                    ui.label(format!("Body of step {current}"));
                });
                if let Some(action) = action {
                    app.actions.push(action);
                }
            },
            App {
                steps,
                current,
                actions: Vec::new(),
            },
        );
    h.run_steps(2);
    h
}

fn click(h: &mut Harness<'static, App>, label: &str) -> Vec<WizardAction> {
    h.state_mut().actions.clear();
    h.get_by_label(label).click();
    h.run_steps(2);
    h.state().actions.clone()
}

#[test]
fn wizard_navigation() {
    // The first step: no Back; Next is disabled while the step is invalid.
    let mut h = harness([false, true, true], 0);
    h.get_by_label("Body of step 0");
    for title in ["1. Source", "2. Target", "3. Review"] {
        h.get_by_label(title);
    }
    assert!(h.get_by_label("Back").accesskit_node().is_disabled());
    assert!(h.get_by_label("Next").accesskit_node().is_disabled());
    assert!(click(&mut h, "Next").is_empty(), "a disabled Next is inert");
    assert_eq!(click(&mut h, "Cancel"), vec![WizardAction::Cancel]);

    // A valid middle step: Back and Next both work.
    let mut h = harness([true, true, true], 1);
    assert_eq!(click(&mut h, "Back"), vec![WizardAction::Back]);
    assert_eq!(click(&mut h, "Next"), vec![WizardAction::Next]);

    // The last step finishes instead, and can be saved as a task.
    let mut h = harness([true, true, true], 2);
    assert!(h.query_by_label("Next").is_none());
    assert_eq!(click(&mut h, "Finish"), vec![WizardAction::Finish]);
    assert_eq!(
        click(&mut h, "Save as task"),
        vec![WizardAction::SaveAsTask]
    );

    // Finish waits for a valid last step.
    let h = harness([true, true, false], 2);
    assert!(h.get_by_label("Finish").accesskit_node().is_disabled());
}
