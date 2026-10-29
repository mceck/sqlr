//! The app's side of agent installations: the worker's lifecycle, the rewrite that follows
//! the server's endpoint, and Settings → Agents' requests.

use std::sync::Arc;

use sqlr_ui::ToastKind;

use super::{
    AgentEnvironment, AgentInstaller, AgentKind, AgentRow, Endpoint, InstallCommand,
    InstallOutcome, Installations,
};
use crate::app::SqlrApp;

/// The agent installation worker as the app holds it.
#[derive(Default)]
pub(crate) struct AgentInstallUi {
    installer: Option<AgentInstaller>,
    /// Replaces the user's directories (tests).
    environment: Option<AgentEnvironment>,
    /// The running endpoint installations were last rewritten for.
    synced: Option<Endpoint>,
    /// The user's directories could not be found.
    unavailable: bool,
}

/// What Settings → Agents shows of the agent list.
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentsView {
    pub rows: Vec<AgentRow>,
    pub executables: Vec<super::AgentExecutable>,
    /// A command is still running; actions wait.
    pub busy: bool,
    /// The home directory is unknown, so no agent can be installed.
    pub unavailable: bool,
}

/// What Settings → Agents asks of the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentRequest {
    /// The section opened: recompute every state.
    Refresh,
    /// Install or Update, already confirmed when the file needed it.
    Install(AgentKind),
    Remove(AgentKind),
    /// Copy the configuration in the agent's format (Manual setup).
    CopyConfig(AgentKind),
}

impl SqlrApp {
    /// Points discovery and installs at another home, for tests.
    #[doc(hidden)]
    pub fn set_agent_environment_for_tests(&mut self, environment: AgentEnvironment) {
        self.agent_installs = AgentInstallUi {
            environment: Some(environment),
            ..Default::default()
        };
    }

    /// The running server's endpoint, or the one it would have on the configured port.
    fn agent_endpoint(&self) -> Endpoint {
        match (self.mcp.url(), self.mcp.token()) {
            (Some(url), Some(token)) => Endpoint::running(url, token),
            _ => Endpoint::stopped(self.settings.agents_port),
        }
    }

    /// The worker, started on first use.
    fn agent_installer(&mut self) -> Option<&AgentInstaller> {
        let installs = &mut self.agent_installs;
        if installs.installer.is_none() && !installs.unavailable {
            let environment = installs
                .environment
                .clone()
                .or_else(AgentEnvironment::from_os);
            match environment {
                Some(environment) => {
                    let repaint: Arc<dyn Fn() + Send + Sync> = match &self.ui_ctx {
                        Some(ctx) => {
                            let ctx = ctx.clone();
                            Arc::new(move || ctx.request_repaint())
                        }
                        None => Arc::new(|| {}),
                    };
                    installs.installer = Some(AgentInstaller::spawn(
                        Installations::new(environment, &self.data_dir),
                        repaint,
                    ));
                }
                None => {
                    tracing::warn!("the home directory is unknown; agents cannot be installed");
                    installs.unavailable = true;
                }
            }
        }
        installs.installer.as_ref()
    }

    /// Each frame: rewrites the recorded installations when the running server's URL or
    /// token changed (token rotation, port change, or either while the server was off), and
    /// turns finished actions into toasts.
    pub(crate) fn agent_installs_tick(&mut self) {
        let endpoint = self.agent_endpoint();
        if endpoint.is_running() && self.agent_installs.synced.as_ref() != Some(&endpoint) {
            self.agent_installs.synced = Some(endpoint.clone());
            if let Some(installer) = self.agent_installer() {
                installer.send(InstallCommand::Rewrite(endpoint));
            }
        }
        let outcomes = self
            .agent_installs
            .installer
            .as_ref()
            .map(AgentInstaller::take_outcomes)
            .unwrap_or_default();
        for outcome in outcomes {
            match outcome {
                InstallOutcome::Installed(kind) => self.notify_as(
                    ToastKind::Success,
                    format!("Restart {} to load sqlr", kind.name()),
                ),
                InstallOutcome::Removed(kind) => self.notify_as(
                    ToastKind::Success,
                    format!("sqlr removed from {}", kind.name()),
                ),
                InstallOutcome::Failed { kind, error } => {
                    self.notify(format!("Could not change {}: {error}", kind.name()));
                }
            }
        }
    }

    /// The agent list for Settings → Agents.
    pub(crate) fn agents_view(&self) -> AgentsView {
        let installs = &self.agent_installs;
        let snapshot = installs
            .installer
            .as_ref()
            .map(AgentInstaller::snapshot)
            .unwrap_or_default();
        AgentsView {
            rows: snapshot.rows,
            executables: snapshot.executables,
            busy: snapshot.pending > 0,
            unavailable: installs.unavailable,
        }
    }

    /// Carries out Settings → Agents' requests: file work goes to the worker.
    pub(crate) fn agent_requests(&mut self, requests: Vec<AgentRequest>, ctx: &egui::Context) {
        for request in requests {
            let endpoint = self.agent_endpoint();
            match request {
                AgentRequest::Refresh => {
                    if let Some(installer) = self.agent_installer() {
                        installer.send(InstallCommand::Refresh(endpoint));
                    }
                }
                AgentRequest::Install(kind) => {
                    if !endpoint.is_running() {
                        self.notify("Start the MCP server before installing an agent");
                        continue;
                    }
                    if let Some(installer) = self.agent_installer() {
                        installer.send(InstallCommand::Install(kind, endpoint));
                    }
                }
                AgentRequest::Remove(kind) => {
                    if let Some(installer) = self.agent_installer() {
                        installer.send(InstallCommand::Remove(kind, endpoint));
                    }
                }
                AgentRequest::CopyConfig(kind) => match endpoint.token() {
                    Some(token) => {
                        ctx.copy_text(kind.config_snippet(&endpoint.url, token));
                        self.notify_as(
                            ToastKind::Info,
                            format!(
                                "{} configuration copied (it contains the token)",
                                kind.name()
                            ),
                        );
                    }
                    None => self.notify("Start the MCP server before copying its configuration"),
                },
            }
        }
    }
}
