//! Guided terminal launches. The structured setting is human-only, just like the custom
//! shell command, and becomes a command only when starting a terminal or showing its preview.

use egui::{TextEdit, Ui};
use serde::{Deserialize, Serialize};
use sqlr_core::{Level, SettingValue};
use sqlr_store::LayeredSettings;
use sqlr_ui::theme::{self, Spacing};

use crate::agent_install::{AgentKind, AgentRequest, AgentsView};
use crate::transfer_dialogs::FileAsk;

#[derive(Clone, Serialize, Deserialize)]
struct AgentLaunch {
    kind: AgentKind,
    executable: String,
    arguments: String,
    return_to_shell: bool,
}

impl AgentLaunch {
    fn read(text: &str) -> Option<Self> {
        let launch: Self = serde_json::from_str(text).ok()?;
        AgentKind::TERMINAL.contains(&launch.kind).then_some(launch)
    }

    fn command(&self) -> String {
        #[cfg(not(windows))]
        let executable = format!("'{}'", self.executable.replace('\'', "'\\''"));
        #[cfg(windows)]
        let executable = format!("call \"{}\"", self.executable.replace('"', ""));
        let mut command = executable;
        if !self.arguments.trim().is_empty() {
            command.push(' ');
            command.push_str(&self.arguments);
        }
        if self.return_to_shell {
            #[cfg(not(windows))]
            command.push_str("\nexec \"${SHELL:-/bin/sh}\" -l");
            #[cfg(windows)]
            command.push_str(" & (where pwsh.exe >nul 2>nul & if errorlevel 1 (where powershell.exe >nul 2>nul & if errorlevel 1 (cmd.exe) else (powershell.exe)) else (pwsh.exe))");
        }
        command
    }
}

/// The command for the next terminal start. Old settings continue to use the custom string.
pub(crate) fn command(settings: &sqlr_store::Settings) -> String {
    AgentLaunch::read(&settings.terminal_launcher).map_or_else(
        || settings.terminal_shell.clone(),
        |launch| launch.command(),
    )
}

/// The selected agent's executable name, retained as each new tab's label.
pub(crate) fn agent_name(settings: &sqlr_store::Settings) -> Option<String> {
    let launch = AgentLaunch::read(&settings.terminal_launcher)?;
    Some(
        std::path::Path::new(&launch.executable)
            .file_name()
            .map_or_else(
                || launch.kind.executable().to_owned(),
                |name| name.to_string_lossy().into_owned(),
            ),
    )
}

#[derive(Default)]
pub(super) struct TerminalPrefs {
    browse: Option<FileAsk>,
}

impl TerminalPrefs {
    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        live: &mut LayeredSettings,
        agents: &AgentsView,
        requests: &mut Vec<AgentRequest>,
        error: &mut Option<String>,
    ) {
        let stored = live.get("terminal.launcher", None).0;
        let mut launch = AgentLaunch::read(stored.as_str());
        let mut shell = live.get("terminal.shell", None).0.as_str().to_string();
        let mut changed = false;
        ui.label("Open with");
        ui.horizontal_wrapped(|ui| {
            for kind in AgentKind::TERMINAL {
                if theme::segment(
                    ui,
                    launch.as_ref().is_some_and(|a| a.kind == kind),
                    kind.name(),
                )
                .clicked()
                {
                    self.select(kind, agents, &mut launch);
                    changed = true;
                }
            }
            if theme::segment(ui, launch.is_none(), "Custom").clicked() {
                changed |= launch.take().is_some();
                self.browse = None;
            }
        });
        ui.add_space(Spacing::M);
        if let Some(agent) = &mut launch {
            theme::section_title(ui, agent.kind.name());
            ui.add_space(Spacing::S);
            ui.horizontal(|ui| {
                let label = ui.label("Executable");
                changed |= sqlr_ui::text_edit(
                    ui,
                    TextEdit::singleline(&mut agent.executable)
                        .desired_width((ui.available_width() - 90.0).max(100.0)),
                )
                .labelled_by(label.id)
                .changed();
                if ui.button("Browse…").clicked() {
                    self.browse = Some(FileAsk::Native(Box::pin(
                        rfd::AsyncFileDialog::new()
                            .set_title("Choose agent executable")
                            .pick_file(),
                    )));
                }
            });
            if let Some(picker) = &mut self.browse
                && let Some(answer) = picker.poll(ui.ctx())
            {
                self.browse = None;
                if let Some(path) = answer {
                    agent.executable = path.to_string_lossy().into_owned();
                    changed = true;
                }
            }
            ui.horizontal(|ui| {
                let label = ui.label("Arguments");
                changed |= sqlr_ui::text_edit(
                    ui,
                    TextEdit::singleline(&mut agent.arguments)
                        .hint_text("Optional command-line arguments")
                        .desired_width(f32::INFINITY),
                )
                .labelled_by(label.id)
                .changed();
            });
            changed |= ui
                .checkbox(
                    &mut agent.return_to_shell,
                    "Return to shell when the agent exits",
                )
                .changed();
            ui.collapsing("Generated command", |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(agent.command()).monospace())
                        .wrap()
                        .selectable(true),
                );
            });
        } else {
            let label = ui.label("Shell command");
            changed |= sqlr_ui::text_edit(
                ui,
                TextEdit::singleline(&mut shell).desired_width(f32::INFINITY),
            )
            .labelled_by(label.id)
            .changed();
            theme::hint(
                ui,
                "Empty uses your login shell. Arguments and shell syntax are supported.",
            );
        }
        ui.add_space(Spacing::S);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!agents.busy, egui::Button::new("Detect again"))
                .clicked()
            {
                requests.push(AgentRequest::Refresh);
            }
            if agents.busy {
                ui.spinner();
                theme::hint(ui, "Detecting agents…");
            } else if agents.unavailable {
                theme::hint(
                    ui,
                    "Discovery is unavailable. Choose an executable with Browse.",
                );
            } else if agents.executables.is_empty() {
                theme::hint(
                    ui,
                    "No agent executable found. Select an agent and use Browse to locate it.",
                );
            }
        });
        theme::hint(
            ui,
            "Changes apply the next time the terminal starts or restarts.",
        );
        ui.add_space(Spacing::L);
        if changed {
            let encoded = launch.as_ref().map(serde_json::to_string).transpose();
            match encoded {
                Ok(encoded) => {
                    let result = live
                        .set(
                            "terminal.launcher",
                            &Level::Global,
                            Some(SettingValue::Text(encoded.unwrap_or_default())),
                        )
                        .and_then(|()| {
                            live.set(
                                "terminal.shell",
                                &Level::Global,
                                Some(SettingValue::Text(shell)),
                            )
                        });
                    *error = result
                        .err()
                        .map(|e| format!("Could not change terminal launch: {e}"));
                }
                Err(_) => *error = Some("Could not save the terminal launch options".into()),
            }
        }
    }

    fn select(&mut self, kind: AgentKind, agents: &AgentsView, launch: &mut Option<AgentLaunch>) {
        if launch.as_ref().is_some_and(|a| a.kind == kind) {
            return;
        }
        *launch = Some(AgentLaunch {
            kind,
            executable: agents
                .executables
                .iter()
                .find(|a| a.kind == kind)
                .map_or_else(
                    || kind.executable().to_string(),
                    |a| a.path.to_string_lossy().into_owned(),
                ),
            arguments: String::new(),
            return_to_shell: true,
        });
        self.browse = None;
    }
}
