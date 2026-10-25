//! Central keyboard dispatch. Editor actions are resolved by the focused editor.
use crate::{app::SqlrApp, palette::AppCommand};
use sqlr_core::{ActionId, Keymap, Scope};

pub fn dispatch_actions(
    ctx: &egui::Context,
    map: &Keymap,
    enabled: impl Fn(ActionId) -> bool,
) -> Vec<ActionId> {
    let mac = ctx.os() == egui::os::OperatingSystem::Mac;
    let mut bindings: Vec<_> = map
        .bindings()
        .iter()
        .filter(|(a, _)| a.scope() == Scope::Global && enabled(*a))
        .collect();
    bindings.sort_by_key(|(_, c)| std::cmp::Reverse(u8::from(c.mods.shift) + u8::from(c.mods.alt)));
    let resolved = sqlr_ui::keymap::UiKeymap::new(map.clone(), mac);
    ctx.input_mut(|i| {
        let mut actions = Vec::new();
        for (action, _) in bindings {
            if !actions.contains(action) && resolved.consume(i, *action) {
                actions.push(*action);
            }
        }
        actions
    })
}
impl SqlrApp {
    pub(crate) fn dispatch_action(&mut self, action: ActionId, ctx: &egui::Context) {
        use ActionId as A;
        let command = match action {
            A::Copy => AppCommand::Copy,
            A::Cut => AppCommand::Cut,
            A::Paste => AppCommand::Paste,
            A::RunStatement => AppCommand::Run,
            A::RunScript => AppCommand::RunScript,
            A::ShowOperations => AppCommand::ShowOperations,
            A::Stop => AppCommand::Stop,
            A::DriverManager => AppCommand::DriverManager,
            A::Quit => AppCommand::Quit,
            A::About => AppCommand::About,
            A::ToggleSoftWrap => AppCommand::ToggleSoftWrap,
            A::Profiles => AppCommand::Profiles,
            A::Shortcuts => AppCommand::Shortcuts,
            A::NextTab | A::PreviousTab => {
                let keys: Vec<_> = self.dock.iter_all_tabs().map(|(_, t)| t.key).collect();
                if let Some(active) = self.active_key()
                    && let Some(index) = keys.iter().position(|k| *k == active)
                {
                    let next = if action == A::NextTab {
                        (index + 1) % keys.len()
                    } else {
                        (index + keys.len() - 1) % keys.len()
                    };
                    self.focus_tab(keys[next]);
                }
                return;
            }
            A::NewTab => AppCommand::NewTab,
            A::CloseTab => AppCommand::CloseTab,
            A::Settings => AppCommand::Settings,
            A::Commit => AppCommand::Commit,
            A::Rollback => AppCommand::Rollback,
            A::Explain => AppCommand::Explain,
            A::ExplainAnalyze => AppCommand::ExplainAnalyze,
            A::ToggleHistory => AppCommand::ToggleHistory,
            A::ToggleValuePanel => AppCommand::ToggleValuePanel,
            A::ToggleTerminal => AppCommand::ToggleTerminal,
            A::NewConnection => AppCommand::NewConnection,
            A::TransferData => AppCommand::TransferData,
            A::CompareSchemas => AppCommand::CompareSchemas,
            A::ExportResult => AppCommand::ExportResult,
            A::SaveSnippet => AppCommand::SaveSnippet,
            A::NewTabOnConnection => {
                self.new_editor_tab();
                return;
            }
            A::OpenObject => {
                self.open_palette(sqlr_ui::PaletteMode::Objects);
                return;
            }
            A::Commands => {
                self.open_palette(sqlr_ui::PaletteMode::Commands);
                return;
            }
            A::ApplyObject => {
                if self.active_is_object()
                    && let Some(key) = self.active_key()
                {
                    self.apply_object_tab(key);
                }
                return;
            }
            _ => return,
        };
        self.run_command(command, ctx);
    }
}

impl AppCommand {
    pub(crate) fn action_id(&self) -> Option<ActionId> {
        use ActionId as A;
        Some(match self {
            Self::DriverManager => A::DriverManager,
            Self::Quit => A::Quit,
            Self::About => A::About,
            Self::ToggleSoftWrap => A::ToggleSoftWrap,
            Self::Profiles => A::Profiles,
            Self::Shortcuts => A::Shortcuts,
            Self::Copy => A::Copy,
            Self::Cut => A::Cut,
            Self::Paste => A::Paste,
            Self::NewTab => A::NewTab,
            Self::CloseTab => A::CloseTab,
            Self::Settings => A::Settings,
            Self::Run => A::RunStatement,
            Self::RunScript => A::RunScript,
            Self::Stop => A::Stop,
            Self::Commit => A::Commit,
            Self::Rollback => A::Rollback,
            Self::Explain => A::Explain,
            Self::ExplainAnalyze => A::ExplainAnalyze,
            Self::ToggleHistory => A::ToggleHistory,
            Self::ToggleValuePanel => A::ToggleValuePanel,
            Self::ToggleTerminal => A::ToggleTerminal,
            Self::NewConnection => A::NewConnection,
            Self::TransferData => A::TransferData,
            Self::CompareSchemas => A::CompareSchemas,
            Self::ExportResult => A::ExportResult,
            Self::SaveSnippet => A::SaveSnippet,
            Self::ShowOperations => A::ShowOperations,
            Self::Undo => A::Undo,
            Self::Redo => A::Redo,
            Self::SelectAll => A::SelectAll,
            Self::NewTabOnConnection => A::NewTabOnConnection,
            Self::OpenObject => A::OpenObject,
            Self::Commands => A::Commands,
            Self::Key(action) => *action,
            _ => return None,
        })
    }
}

impl AppCommand {
    /// The icon the command palette and menus show for the command.
    pub(crate) fn icon(&self) -> &'static str {
        use sqlr_ui::theme::icons as i;
        match self {
            Self::NewTab | Self::NewTabOnConnection => i::QUERY,
            Self::CloseTab => i::CLOSE,
            Self::NewConnection | Self::Connect(_) => i::CONNECTION,
            Self::Disconnect(_) => i::DISCONNECT,
            Self::ImportConnections => i::IMPORT,
            Self::ExportConnections | Self::ExportResult => i::EXPORT,
            Self::Settings => i::SETTINGS,
            Self::DriverManager => i::DRIVERS,
            Self::Quit => i::QUIT,
            Self::About | Self::Diagnostics | Self::ReportProblem => i::WARNING,
            Self::Run => i::RUN,
            Self::RunScript => i::RUN_SCRIPT,
            Self::Stop => i::STOP,
            Self::ToggleValuePanel => i::PANEL,
            Self::ToggleTerminal => i::TERMINAL,
            Self::ToggleHistory => i::HISTORY,
            Self::ToggleSoftWrap => i::SOFT_WRAP,
            Self::SaveSnippet => i::SNIPPET,
            Self::Commit => i::COMMIT,
            Self::Rollback => i::ROLLBACK,
            Self::Profiles => i::PROFILES,
            Self::NewObject(_, kind) => i::for_object(*kind),
            Self::EditSelected(..) => i::EDIT,
            Self::OpenAdmin(..) => i::SESSIONS,
            Self::TransferData => i::TRANSFER,
            Self::ShowOperations => i::TIMER,
            Self::CompareSchemas => i::COMPARE,
            Self::Explain => i::PLAN,
            Self::ExplainAnalyze => i::TIMER,
            Self::Undo => i::UNDO,
            Self::Redo => i::REDO,
            Self::Cut => i::CUT,
            Self::Copy => i::COPY,
            Self::Paste => i::PASTE,
            Self::Delete => i::DELETE,
            Self::SelectAll => i::SELECT_ALL,
            Self::Shortcuts => i::KEYBOARD,
            Self::CheckUpdates => i::UPDATES,
            Self::NewIndex(..) => i::KEY,
            Self::OpenObject => i::GO_TO,
            Self::Commands => i::COMMAND,
            Self::Key(_) => i::COMMAND,
        }
    }
}
