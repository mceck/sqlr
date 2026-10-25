//! The parts of a multi-step wizard shown in a [`theme::Dialog`] (the data transfer wizard,
//! saved-task editing): the step strip ([`wizard_steps`], in the dialog's top band) and the
//! Cancel / Back / Next actions ([`wizard_footer`], in its footer). The caller owns the
//! steps and the current index and acts on the returned [`WizardAction`].

use egui::RichText;

use crate::theme::{self, Spacing};

/// One step of a wizard. `valid` says whether the user may move past it; `reason` says
/// what is missing when it is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WizardStep {
    pub title: String,
    pub valid: bool,
    pub reason: Option<String>,
}

impl WizardStep {
    /// A step that may be left once `missing` is `None`; otherwise `missing` says why not.
    pub fn new(title: impl Into<String>, missing: Option<String>) -> Self {
        Self {
            title: title.into(),
            valid: missing.is_none(),
            reason: missing,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardAction {
    Back,
    Next,
    Finish,
    Cancel,
    SaveAsTask,
}

/// The step strip: "1. Source › 2. Target › …", the current step highlighted.
pub fn wizard_steps(ui: &mut egui::Ui, steps: &[WizardStep], current: usize) {
    let p = theme::palette(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        for (i, step) in steps.iter().enumerate() {
            let text = RichText::new(format!("{}. {}", i + 1, step.title));
            let text = match i.cmp(&current) {
                std::cmp::Ordering::Equal => text.color(p.accent).strong(),
                std::cmp::Ordering::Less => text.color(p.text),
                std::cmp::Ordering::Greater => text.color(p.text_muted),
            };
            egui::Frame::new()
                .fill(if i == current {
                    p.accent_soft
                } else {
                    egui::Color32::TRANSPARENT
                })
                .corner_radius(Spacing::RADIUS_S)
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.label(text);
                });
            if i + 1 < steps.len() {
                ui.label(RichText::new("›").color(p.text_muted));
            }
        }
    });
}

/// The wizard's actions in a dialog footer: "Save as task" on the left of the last step,
/// then Cancel, Back and Next on the right. Next is disabled, with the step's reason,
/// until the current step is valid; on the last step it becomes Finish.
pub fn wizard_footer(
    f: &mut theme::DialogFooter,
    steps: &[WizardStep],
    current: usize,
) -> Option<WizardAction> {
    let last = current + 1 >= steps.len();
    let step = steps.get(current);
    let blocked = match step {
        Some(s) if s.valid => None,
        Some(s) => Some(s.reason.as_deref().unwrap_or("")),
        None => Some(""),
    };
    let mut action = None;
    if last && f.secondary("Save as task", true) {
        action = Some(WizardAction::SaveAsTask);
    }
    let (label, forward) = if last {
        ("Finish", WizardAction::Finish)
    } else {
        ("Next", WizardAction::Next)
    };
    if f.primary(label, blocked) {
        action = Some(forward);
    }
    if f.button("Back", current > 0) {
        action = Some(WizardAction::Back);
    }
    if f.cancel() {
        action = Some(WizardAction::Cancel);
    }
    action
}
