//! Saved tasks in the app (spec §8.5): the Tasks pane, running a task (connecting what it
//! needs first), the "Save as task" name prompt and a run's log.

use std::time::{SystemTime, UNIX_EPOCH};

use egui::TextEdit;
use sqlr_core::{SavedTask, TaskRun, TaskSpec};
use sqlr_engine::{JobId, JobOutcome, SessionStatus};
use sqlr_ui::theme::{self, Spacing};
use sqlr_ui::{RunningTask, TasksAction, ToastKind, tasks_panel};

use crate::app::{LeftPane, SqlrApp};
use crate::transfer_wizard::TransferWizard;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// The "Save as task" prompt: a name for a spec, or an edited task's name to keep.
struct NamePrompt {
    spec: TaskSpec,
    /// The id and name of the task being edited.
    editing: Option<(String, String)>,
    name: String,
}

#[derive(Default)]
pub(crate) struct TasksUi {
    selected: Option<String>,
    runs: Vec<TaskRun>,
    /// The task `runs` belong to.
    runs_for: Option<String>,
    runs_job: Option<JobId>,
    refresh_runs: bool,
    /// Tasks started and their jobs.
    running: Vec<(String, JobId)>,
    /// Tasks waiting for their connections to come up.
    pending: Vec<String>,
    /// A run's log: its title and text.
    log: Option<(String, String)>,
    name_prompt: Option<NamePrompt>,
}

/// A name to start the prompt with.
fn default_name(spec: &TaskSpec) -> String {
    match spec {
        TaskSpec::Dump { database, .. } => format!("Dump {database}"),
        TaskSpec::Restore { database, .. } => format!("Restore {database}"),
        TaskSpec::Transfer(_) => "Transfer data".to_string(),
        TaskSpec::Script { .. } => "Script".to_string(),
    }
}

impl SqlrApp {
    /// Asks for a name and saves `spec` as a task; with `editing` the task keeps its name
    /// and is replaced without asking.
    pub(crate) fn save_task_dialog(&mut self, spec: TaskSpec, editing: Option<(String, String)>) {
        match editing {
            Some((id, name)) => self.store_task(id, name, spec, true),
            None => {
                self.tasks_ui.name_prompt = Some(NamePrompt {
                    name: default_name(&spec),
                    spec,
                    editing: None,
                });
            }
        }
    }

    fn store_task(&mut self, id: String, name: String, spec: TaskSpec, replaced: bool) {
        let created_ms = self
            .engine
            .tasks()
            .iter()
            .find(|t| t.id == id)
            .map_or_else(now_ms, |t| t.created_ms);
        self.engine.save_task(SavedTask {
            id: id.clone(),
            name: name.clone(),
            spec,
            created_ms,
            updated_ms: now_ms(),
        });
        self.tasks_ui.selected = Some(id);
        self.tasks_ui.refresh_runs = true;
        self.notify_as(
            ToastKind::Success,
            if replaced {
                format!("Updated task {name}")
            } else {
                format!("Saved task {name}")
            },
        );
    }

    /// The Tasks pane of the left panel.
    pub(crate) fn tasks_pane(&mut self, ui: &mut egui::Ui) {
        let ui_state = &mut self.tasks_ui;
        if ui_state.selected != ui_state.runs_for || ui_state.refresh_runs {
            if ui_state.selected != ui_state.runs_for {
                ui_state.runs.clear();
            }
            ui_state.runs_for = ui_state.selected.clone();
            ui_state.refresh_runs = false;
            if let Some(old) = ui_state.runs_job.take() {
                self.engine.forget_job(old);
            }
            if let Some(id) = &ui_state.selected {
                ui_state.runs_job = Some(self.engine.task_runs(id));
            }
        }
        if let Some(job) = self.tasks_ui.runs_job
            && let Some(state) = self.engine.job(job)
            && let Some(outcome) = state.outcome
        {
            self.engine.forget_job(job);
            self.tasks_ui.runs_job = None;
            if let JobOutcome::TaskRuns(runs) = outcome {
                self.tasks_ui.runs = runs;
            }
        }
        let tasks = self.engine.tasks();
        let running: Vec<RunningTask> = self
            .tasks_ui
            .running
            .iter()
            .map(|(task, job)| {
                let progress = self
                    .engine
                    .job(*job)
                    .map_or(String::new(), |s| format!("{} rows", s.progress.rows));
                RunningTask {
                    task_id: task.clone(),
                    job: job.0,
                    progress,
                }
            })
            .collect();
        let action = tasks_panel(
            ui,
            &tasks,
            &running,
            &self.tasks_ui.runs,
            &mut self.tasks_ui.selected,
        );
        if let Some(action) = action {
            self.tasks_action(action, &tasks);
        }
    }

    fn tasks_action(&mut self, action: TasksAction, tasks: &[SavedTask]) {
        let find = |id: &str| tasks.iter().find(|t| t.id == id).cloned();
        match action {
            TasksAction::Run(id) => {
                if !self.tasks_ui.pending.contains(&id) {
                    self.tasks_ui.pending.push(id);
                }
            }
            TasksAction::Edit(id) => {
                if let Some(task) = find(&id) {
                    self.edit_task(task);
                }
            }
            TasksAction::Duplicate(id) => {
                if let Some(task) = find(&id) {
                    self.store_task(
                        uuid::Uuid::new_v4().to_string(),
                        format!("{} (copy)", task.name),
                        task.spec,
                        false,
                    );
                }
            }
            TasksAction::Delete(id) => {
                self.engine.delete_task(&id);
                if self.tasks_ui.selected.as_deref() == Some(id.as_str()) {
                    self.tasks_ui.selected = None;
                }
            }
            TasksAction::ShowRun(run_id) => {
                if let Some(run) = self.tasks_ui.runs.iter().find(|r| r.id == run_id) {
                    let name = self
                        .tasks_ui
                        .selected
                        .as_deref()
                        .and_then(find)
                        .map_or_else(String::new, |t| t.name);
                    let text = if run.log.is_empty() {
                        run.summary.clone()
                    } else {
                        format!("{}\n\n{}", run.summary, run.log)
                    };
                    self.tasks_ui.log = Some((format!("{name} · {}", run.outcome), text));
                }
            }
            TasksAction::Cancel(job) => self.engine.cancel_job(JobId(job)),
        }
    }

    /// Reopens a task in the wizard or dialog that made it.
    fn edit_task(&mut self, task: SavedTask) {
        match &task.spec {
            TaskSpec::Transfer(_) => {
                let conns = self.wizard_connections();
                self.transfer_wizard = TransferWizard::from_task(&conns, &task);
            }
            TaskSpec::Dump { .. } => {
                self.dump_dialog = crate::database_tools::DumpDialog::from_task(self, &task);
            }
            TaskSpec::Restore { .. } => {
                self.restore_dialog = crate::database_tools::RestoreDialog::from_task(self, &task);
            }
            TaskSpec::Script { .. } => {
                self.notify("Script tasks cannot be edited yet".to_string());
            }
        }
    }

    /// Shows the Tasks pane (⌘K "Tasks").
    pub(crate) fn show_tasks_pane(&mut self) {
        self.left = LeftPane::Tasks;
    }

    /// Per frame: starts the tasks waiting for their connections and reports the ones that
    /// ended.
    pub(crate) fn poll_tasks(&mut self) {
        let pending = std::mem::take(&mut self.tasks_ui.pending);
        let tasks = self.engine.tasks();
        for id in pending {
            let Some(task) = tasks.iter().find(|t| t.id == id) else {
                continue;
            };
            let mut waiting = false;
            let mut failed = None;
            for conn in task.spec.connections() {
                if self.connection(&conn).is_none() {
                    failed = Some(format!(
                        "The connection of task {} no longer exists: edit the task",
                        task.name
                    ));
                    break;
                }
                match self.engine.session_status(&conn) {
                    Some(SessionStatus::Connected(_)) => {}
                    Some(SessionStatus::Failed(e)) => {
                        failed = Some(format!(
                            "Could not connect for {}: {}",
                            task.name, e.message
                        ));
                        break;
                    }
                    Some(SessionStatus::Connecting) => waiting = true,
                    Some(SessionStatus::Disconnected) | None => {
                        waiting = true;
                        self.connect(&conn);
                    }
                }
            }
            if let Some(message) = failed {
                self.notify_as(ToastKind::Error, message);
            } else if waiting {
                self.tasks_ui.pending.push(id);
            } else {
                match self.engine.run_task(&id) {
                    Ok(job) => {
                        self.tasks_ui.running.push((id, job));
                        self.tasks_ui.refresh_runs = true;
                    }
                    Err(e) => self.notify_as(ToastKind::Error, format!("{}: {e}", task.name)),
                }
            }
        }
        let mut ended = Vec::new();
        self.tasks_ui.running.retain(|(task, job)| {
            match self.engine.job(*job).and_then(|j| j.outcome) {
                Some(outcome) => {
                    ended.push((task.clone(), *job, outcome));
                    false
                }
                None => self.engine.job(*job).is_some(),
            }
        });
        for (task, job, outcome) in ended {
            self.engine.forget_job(job);
            let name = tasks
                .iter()
                .find(|t| t.id == task)
                .map_or(task, |t| t.name.clone());
            let (kind, text) = match outcome {
                JobOutcome::Failed(e) => {
                    (ToastKind::Error, format!("{name} failed: {}", e.message))
                }
                JobOutcome::Cancelled => (ToastKind::Info, format!("{name} cancelled")),
                _ => (ToastKind::Success, format!("{name} finished")),
            };
            self.notify_as(kind, text);
            self.tasks_ui.refresh_runs = true;
        }
    }

    /// The name prompt and the run log windows.
    pub(crate) fn tasks_windows(&mut self, ctx: &egui::Context) {
        if let Some(prompt) = &mut self.tasks_ui.name_prompt {
            let mut save = false;
            let mut close = false;
            egui::Window::new("Save as task")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Name");
                    let edit = ui.add(TextEdit::singleline(&mut prompt.name).desired_width(280.0));
                    let valid = !prompt.name.trim().is_empty();
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && valid {
                        save = true;
                    }
                    ui.add_space(Spacing::M);
                    ui.horizontal(|ui| {
                        let button = theme::primary_button_state(ui.ctx(), valid, "Save task");
                        if ui.add_enabled(valid, button).clicked() {
                            save = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            if save {
                if let Some(prompt) = self.tasks_ui.name_prompt.take() {
                    let id = prompt
                        .editing
                        .map_or_else(|| uuid::Uuid::new_v4().to_string(), |(id, _)| id);
                    self.store_task(id, prompt.name.trim().to_string(), prompt.spec, false);
                }
            } else if close {
                self.tasks_ui.name_prompt = None;
            }
        }
        if let Some((title, text)) = &self.tasks_ui.log {
            let mut open = true;
            let mut copy = false;
            let mut shown = text.as_str();
            egui::Window::new(format!("Run log · {title}"))
                .open(&mut open)
                .default_size([640.0, 380.0])
                .show(ctx, |ui| {
                    if ui.button("Copy").clicked() {
                        copy = true;
                    }
                    egui::ScrollArea::both().show(ui, |ui| {
                        ui.add(
                            TextEdit::multiline(&mut shown)
                                .code_editor()
                                .desired_width(f32::INFINITY),
                        );
                    });
                });
            if copy {
                ctx.copy_text(text.clone());
            }
            if !open {
                self.tasks_ui.log = None;
            }
        }
    }
}
