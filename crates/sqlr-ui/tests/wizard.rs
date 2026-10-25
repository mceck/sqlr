use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sqlr_ui::theme::{self, Dialog, Palette, icons};
use sqlr_ui::{WizardAction, WizardStep, wizard_footer, wizard_steps};

struct App {
    steps: Vec<WizardStep>,
    current: usize,
    actions: Vec<WizardAction>,
}

fn harness(valid: [bool; 3], current: usize) -> Harness<'static, App> {
    let steps = ["Source", "Target", "Review"]
        .iter()
        .zip(valid)
        .map(|(title, valid)| {
            WizardStep::new(*title, (!valid).then(|| format!("Complete {title}")))
        })
        .collect();
    let mut h = Harness::builder()
        .with_size(vec2(900.0, 600.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, app: &mut App| {
                theme::apply(ui.ctx(), &Palette::dark(), 13.0);
                let current = app.current;
                let response =
                    Dialog::new("wizard", icons::ROWS, "Transfer data").show(ui.ctx(), |d| {
                        d.top(|ui| wizard_steps(ui, &app.steps, current));
                        d.body(|ui| {
                            ui.label(format!("Body of step {current}"));
                        });
                        d.footer(|f| wizard_footer(f, &app.steps, current))
                    });
                if let Some(action) = response.inner {
                    app.actions.push(action);
                }
            },
            App {
                steps,
                current,
                actions: Vec::new(),
            },
        );
    h.run_steps(3);
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
    // The first step: Back is disabled; Next is disabled, with the reason, while the step
    // is invalid.
    let mut h = harness([false, true, true], 0);
    h.get_by_label("Body of step 0");
    for title in ["1. Source", "2. Target", "3. Review"] {
        h.get_by_label(title);
    }
    assert!(h.get_by_label("Back").accesskit_node().is_disabled());
    assert!(h.get_by_label("Next").accesskit_node().is_disabled());
    h.get_by_label("Complete Source");
    assert!(click(&mut h, "Next").is_empty(), "a disabled Next is inert");
    assert_eq!(click(&mut h, "Cancel"), vec![WizardAction::Cancel]);

    // A valid middle step: Back and Next both work; Cancel, Back and Next read left to right.
    let mut h = harness([true, true, true], 1);
    let cancel = h.get_by_label("Cancel").rect();
    let back = h.get_by_label("Back").rect();
    let next = h.get_by_label("Next").rect();
    assert!(cancel.right() < back.left() && back.right() < next.left());
    assert_eq!(click(&mut h, "Back"), vec![WizardAction::Back]);
    assert_eq!(click(&mut h, "Next"), vec![WizardAction::Next]);

    // The last step finishes without offering task persistence.
    let mut h = harness([true, true, true], 2);
    assert!(h.query_by_label("Next").is_none());
    assert_eq!(click(&mut h, "Finish"), vec![WizardAction::Finish]);
    assert!(h.query_by_label("Save as task").is_none());

    // Finish waits for a valid last step.
    let h = harness([true, true, false], 2);
    assert!(h.get_by_label("Finish").accesskit_node().is_disabled());
}
