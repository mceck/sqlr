//! The Tasks panel (spec §8.5): saved tasks by kind, the actions on the selected one, and
//! its run history. The caller does the work for the returned [`TasksAction`].

use egui::{RichText, Sense};
use sqlr_core::{SavedTask, TaskRun};

use crate::history::{format_duration, format_when};
use crate::theme::{self, Spacing};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TasksAction {
    Run(String),
    Edit(String),
    Duplicate(String),
    Delete(String),
    /// Show a run's log.
    ShowRun(i64),
    /// Cancel a running task's job (the job's id).
    Cancel(u64),
}

/// A task that is running now: its job and a progress line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningTask {
    pub task_id: String,
    pub job: u64,
    pub progress: String,
}

const KINDS: [(&str, &str); 4] = [
    ("transfer", "Transfers"),
    ("dump", "Dumps"),
    ("restore", "Restores"),
    ("script", "Scripts"),
];

/// Draws the panel: the task list (selecting sets `selected`), the toolbar for the selected
/// task, and its `runs` (newest first, the caller loads them for the selection).
pub fn tasks_panel(
    ui: &mut egui::Ui,
    tasks: &[SavedTask],
    running: &[RunningTask],
    runs: &[TaskRun],
    selected: &mut Option<String>,
) -> Option<TasksAction> {
    let mut action = None;
    let p = theme::palette(ui.ctx());
    if selected
        .as_ref()
        .is_some_and(|id| !tasks.iter().any(|t| &t.id == id))
    {
        *selected = None;
    }
    let current = selected.clone();
    let running_here = current
        .as_ref()
        .and_then(|id| running.iter().find(|r| &r.task_id == id));
    ui.horizontal(|ui| {
        let enabled = current.is_some();
        let can_run = enabled && running_here.is_none();
        if let Some(id) = &current {
            if ui
                .add_enabled(can_run, egui::Button::new("Run task"))
                .clicked()
            {
                action = Some(TasksAction::Run(id.clone()));
            }
            if ui.add_enabled(enabled, egui::Button::new("Edit")).clicked() {
                action = Some(TasksAction::Edit(id.clone()));
            }
            if ui
                .add_enabled(enabled, egui::Button::new("Duplicate"))
                .clicked()
            {
                action = Some(TasksAction::Duplicate(id.clone()));
            }
            if ui
                .add_enabled(enabled, egui::Button::new("Delete"))
                .clicked()
            {
                action = Some(TasksAction::Delete(id.clone()));
            }
            if let Some(r) = running_here
                && ui.button("Cancel").clicked()
            {
                action = Some(TasksAction::Cancel(r.job));
            }
        } else {
            for label in ["Run task", "Edit", "Duplicate", "Delete"] {
                ui.add_enabled(false, egui::Button::new(label));
            }
        }
    });
    ui.add_space(Spacing::S);
    if tasks.is_empty() {
        ui.label(
            RichText::new(
                "No tasks yet. Save a transfer, dump or restore as a task from its last step.",
            )
            .color(p.text_muted),
        );
    }
    egui::ScrollArea::vertical()
        .id_salt("tasks-list")
        .max_height(ui.available_height() * 0.5)
        .show(ui, |ui| {
            for (kind, title) in KINDS {
                let group: Vec<&SavedTask> =
                    tasks.iter().filter(|t| t.spec.kind() == kind).collect();
                if group.is_empty() {
                    continue;
                }
                ui.label(RichText::new(title).small().color(p.text_muted));
                for task in group {
                    let is_selected = current.as_deref() == Some(task.id.as_str());
                    let busy = running.iter().any(|r| r.task_id == task.id);
                    let text = if busy {
                        format!("{} (running)", task.name)
                    } else {
                        task.name.clone()
                    };
                    if ui.selectable_label(is_selected, text).clicked() {
                        *selected = Some(task.id.clone());
                    }
                }
            }
        });
    if let Some(r) = running_here {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(&r.progress);
        });
    }
    ui.add_space(Spacing::M);
    if current.is_some() {
        theme::hairline(ui);
        ui.label(RichText::new("Runs").strong());
        if runs.is_empty() {
            ui.label(RichText::new("Never run").color(p.text_muted));
        }
        egui::ScrollArea::vertical()
            .id_salt("tasks-runs")
            .show(ui, |ui| {
                for run in runs {
                    let color = match run.outcome.as_str() {
                        "ok" => p.success,
                        "failed" => p.error,
                        _ => p.text_muted,
                    };
                    let took = run
                        .ended_ms
                        .map(|end| format_duration((end - run.started_ms).max(0) as u64))
                        .unwrap_or_else(|| "…".to_string());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&run.outcome).color(color).small());
                        ui.label(
                            RichText::new(format!("{} · {took}", format_when(run.started_ms)))
                                .small()
                                .color(p.text_muted),
                        );
                    });
                    let summary = ui.add(
                        egui::Label::new(if run.summary.is_empty() {
                            "(no summary)".to_string()
                        } else {
                            run.summary.clone()
                        })
                        .sense(Sense::click()),
                    );
                    if summary.clicked() {
                        action = Some(TasksAction::ShowRun(run.id));
                    }
                    ui.add_space(Spacing::S);
                }
            });
    }
    action
}
