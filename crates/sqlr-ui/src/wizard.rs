//! The frame of a multi-step wizard (the data transfer wizard, saved-task editing): a step
//! strip, the step's body and a Cancel / Back / Next bar. The caller owns the steps and
//! the current index and acts on the returned [`WizardAction`].

use egui::{Align, Layout, RichText};

use crate::theme::{self, Spacing};

/// One step of a wizard. `valid` says whether the user may move past it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WizardStep {
    pub title: String,
    pub valid: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardAction {
    Back,
    Next,
    Finish,
    Cancel,
    SaveAsTask,
}

/// Draws the step strip, `body` and the button bar; returns the button the user clicked.
/// Next is disabled until the current step is valid; on the last step it becomes Finish
/// (also gated on validity) and "Save as task" appears.
pub fn wizard_frame(
    ui: &mut egui::Ui,
    steps: &[WizardStep],
    current: usize,
    body: impl FnOnce(&mut egui::Ui),
) -> Option<WizardAction> {
    let p = theme::palette(ui.ctx());
    let last = current + 1 >= steps.len();
    let valid = steps.get(current).is_some_and(|s| s.valid);
    let mut action = None;

    ui.horizontal_wrapped(|ui| {
        for (i, step) in steps.iter().enumerate() {
            let text = RichText::new(format!("{}. {}", i + 1, step.title));
            let text = match i.cmp(&current) {
                std::cmp::Ordering::Equal => text.color(p.accent).strong(),
                std::cmp::Ordering::Less => text.color(p.text),
                std::cmp::Ordering::Greater => text.color(p.text_muted),
            };
            ui.label(text);
            if i + 1 < steps.len() {
                ui.label(RichText::new("›").color(p.text_muted));
            }
        }
    });
    theme::hairline(ui);
    ui.add_space(Spacing::M);

    // The bar sits under the body, so reserve its height first.
    // Over-reserved on purpose: a window that grows to fit would grow on every frame.
    let bar_height = 56.0;
    let body_height = (ui.available_height() - bar_height - Spacing::M).max(0.0);
    egui::ScrollArea::vertical()
        .id_salt("wizard-body")
        .max_height(body_height)
        .auto_shrink([false, false])
        .show(ui, body);

    ui.add_space(Spacing::M);
    theme::hairline(ui);
    ui.add_space(Spacing::XS);
    ui.horizontal(|ui| {
        if ui.button("Cancel").clicked() {
            action = Some(WizardAction::Cancel);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if last {
                let finish = ui.add_enabled(
                    valid,
                    theme::primary_button_state(ui.ctx(), valid, "Finish"),
                );
                if finish.clicked() {
                    action = Some(WizardAction::Finish);
                }
            } else {
                let next =
                    ui.add_enabled(valid, theme::primary_button_state(ui.ctx(), valid, "Next"));
                if next.clicked() {
                    action = Some(WizardAction::Next);
                }
            }
            if ui
                .add_enabled(current > 0, egui::Button::new("Back"))
                .clicked()
            {
                action = Some(WizardAction::Back);
            }
            if last && ui.button("Save as task").clicked() {
                action = Some(WizardAction::SaveAsTask);
            }
        });
    });
    action
}
