//! The main toolbar: app mark and menus, the connection chip, the execution and transaction
//! controls, then the workspace toggles on the right. In a narrow window the menus fold into
//! one button and the transaction controls into an overflow button, so nothing overlaps or
//! leaves the window.

use egui::{Align, FontId, Id, Layout, PopupCloseBehavior, Stroke, TextStyle};
use sqlr_core::{ActionId, ConnectionConfig};
use sqlr_engine::SessionStatus;
use sqlr_ui::theme::{self, Spacing, icons, menu};
use sqlr_ui::{ChipAction, ChipItem, ChipState, PaletteMode, connection_chip};

use crate::agents::McpStatus;
use crate::app::{SqlrApp, tab_mut};
use crate::palette::AppCommand;

/// Widths the toolbar budgets before it has measured its groups, in points.
const MENUS_ESTIMATE: f32 = 220.0;
const TX_ESTIMATE: f32 = 300.0;
/// The chip's narrowest and widest sizes in the toolbar.
const CHIP_MIN: f32 = 96.0;
const CHIP_MAX: f32 = 220.0;

/// One row of the in-window menus: a label, the shortcut hint of its action from the keymap
/// and the command it runs. The same [`AppCommand`] dispatcher serves the native menu, so the
/// two menus cannot drift apart.
fn command_item(ui: &mut egui::Ui, app: &mut SqlrApp, label: &str, command: AppCommand) {
    let shortcut = command
        .action_id()
        .and_then(|a| sqlr_ui::keymap::shortcut(ui.ctx(), a));
    // A keyboard-only action is reached through its shortcut: none, nothing to press.
    let enabled = !matches!(command, AppCommand::Key(_)) || shortcut.is_some();
    // The shortcut is decoration: the menu entry keeps its plain label for accessibility
    // and for tests.
    let response = menu::MenuItem::new(label)
        .shortcut(shortcut)
        .enabled(enabled)
        .ui(ui);
    let response = if enabled {
        response
    } else {
        response.on_disabled_hover_text("Give it a shortcut in Preferences → Keymap")
    };
    if response.clicked() {
        app.run_command(command, ui.ctx());
    }
}

/// A View menu choice with its toggle's shortcut; true when clicked.
fn check_item(ui: &mut egui::Ui, checked: bool, label: &str, action: ActionId) -> bool {
    menu::MenuItem::new(label)
        .checked(checked)
        .shortcut(sqlr_ui::keymap::shortcut(ui.ctx(), action))
        .ui(ui)
        .clicked()
}

/// The width a toolbar group took last frame, or `estimate` before it was drawn.
fn measured(ui: &egui::Ui, id: Id, estimate: f32) -> f32 {
    ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(estimate)
}

/// Draws `add` in a horizontal group and remembers its width for [`measured`].
fn group<R>(ui: &mut egui::Ui, id: Id, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let inner = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        add(ui)
    });
    let width = inner.response.rect.width();
    ui.data_mut(|d| d.insert_temp(id, width));
    inner.inner
}

/// The chip state of a session status.
fn chip_state(status: Option<&SessionStatus>) -> ChipState {
    match status {
        Some(SessionStatus::Connected(_)) => ChipState::Connected,
        Some(SessionStatus::Connecting) => ChipState::Connecting,
        Some(SessionStatus::Failed(_)) => ChipState::Failed,
        Some(SessionStatus::Disconnected) | None => ChipState::Offline,
    }
}

impl SqlrApp {
    fn menu(&mut self, ui: &mut egui::Ui) {
        use AppCommand as C;
        menu::bar_menu(ui, "File", |ui| {
            command_item(ui, self, "New query tab", C::NewTab);
            command_item(
                ui,
                self,
                "New query tab on the current connection",
                C::NewTabOnConnection,
            );
            command_item(ui, self, "Close tab", C::CloseTab);
            ui.separator();
            command_item(ui, self, "New connection", C::NewConnection);
            command_item(ui, self, "Connection profiles…", C::Profiles);
            ui.separator();
            command_item(ui, self, "Import…", C::ImportConnections);
            command_item(ui, self, "Export…", C::ExportConnections);
            ui.separator();
            command_item(ui, self, "Settings…", C::Settings);
            ui.separator();
            command_item(ui, self, "Operations", C::ShowOperations);
            command_item(ui, self, "Quit", C::Quit);
        });
        menu::bar_menu(ui, "Edit", |ui| {
            command_item(ui, self, "Undo", C::Undo);
            command_item(ui, self, "Redo", C::Redo);
            ui.separator();
            command_item(ui, self, "Cut", C::Cut);
            command_item(ui, self, "Copy", C::Copy);
            command_item(ui, self, "Paste", C::Paste);
            command_item(ui, self, "Delete", C::Delete);
            ui.separator();
            command_item(ui, self, "Select All", C::SelectAll);
            ui.separator();
            command_item(ui, self, "Find…", C::Key(ActionId::Find));
            command_item(ui, self, "Find and replace…", C::Key(ActionId::Replace));
            command_item(
                ui,
                self,
                "Toggle line comment",
                C::Key(ActionId::ToggleComment),
            );
            command_item(ui, self, "Copy line down", C::Key(ActionId::DuplicateLine));
            ui.separator();
            command_item(ui, self, "Rename…", C::Key(ActionId::RenameSelection));
            command_item(
                ui,
                self,
                "Delete selected rows",
                C::Key(ActionId::DeleteRows),
            );
        });
        menu::bar_menu(ui, "View", |ui| {
            command_item(ui, self, "Command palette…", C::Commands);
            command_item(ui, self, "Go to a table or view…", C::OpenObject);
            ui.separator();
            if check_item(
                ui,
                self.show_value_panel,
                "Value panel",
                ActionId::ToggleValuePanel,
            ) {
                self.show_value_panel = !self.show_value_panel;
            }
            if check_item(ui, self.show_history, "History", ActionId::ToggleHistory) {
                self.show_history = !self.show_history;
            }
            if check_item(
                ui,
                self.terminal.is_open(),
                "Terminal",
                ActionId::ToggleTerminal,
            ) {
                self.terminal.toggle();
            }
            if check_item(
                ui,
                self.settings.soft_wrap,
                "Soft wrap",
                ActionId::ToggleSoftWrap,
            ) {
                self.toggle_soft_wrap(ui.ctx());
            }
            ui.separator();
        });
        menu::bar_menu(ui, "Query", |ui| {
            command_item(ui, self, "Run statement", C::Run);
            command_item(ui, self, "Run script", C::RunScript);
            command_item(ui, self, "Stop", C::Stop);
            ui.separator();
            command_item(ui, self, "Explain", C::Explain);
            command_item(ui, self, "Explain analyze", C::ExplainAnalyze);
            ui.separator();
            command_item(ui, self, "Commit", C::Commit);
            command_item(ui, self, "Rollback", C::Rollback);
            ui.separator();
            command_item(ui, self, "Format SQL", C::Key(ActionId::FormatSql));
            command_item(ui, self, "Save as snippet…", C::SaveSnippet);
        });
        menu::bar_menu(ui, "Tools", |ui| {
            command_item(ui, self, "Transfer data…", C::TransferData);
            command_item(ui, self, "Compare schemas…", C::CompareSchemas);
            command_item(ui, self, "Export result…", C::ExportResult);
            ui.separator();
            command_item(ui, self, "Drivers…", C::DriverManager);
            ui.separator();
            menu::hint(
                ui,
                "Import, generate data, ER diagrams, dump and restore: right-click a table or \
                 database in the explorer.",
            );
        });
        menu::bar_menu(ui, "Help", |ui| {
            command_item(ui, self, "Keyboard Shortcuts", C::Shortcuts);
            ui.separator();
            command_item(ui, self, "Check for Updates…", C::CheckUpdates);
            command_item(ui, self, "Diagnostics…", C::Diagnostics);
            command_item(ui, self, "Report a Problem…", C::ReportProblem);
        });
    }

    /// Whether the menus live in the system menu bar, so the toolbar should not repeat
    /// them.
    fn native_menu_installed(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.native_menu.is_some()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    /// The chip's entry for a saved connection: its state and driver name.
    fn chip_item(&self, c: &ConnectionConfig) -> ChipItem {
        ChipItem {
            id: c.id.clone(),
            name: c.name.clone(),
            detail: self
                .engine
                .drivers()
                .get(&c.driver)
                .map_or_else(|| c.driver.clone(), |d| d.manifest().name.clone()),
            driver: c.driver.clone(),
            color: self
                .connection_tint(&c.id)
                .map(|[r, g, b]| egui::Color32::from_rgb(r, g, b)),
            state: chip_state(self.engine.session_status(&c.id).as_ref()),
        }
    }

    fn chip_action(&mut self, action: ChipAction, active: Option<u64>, current: Option<String>) {
        match action {
            ChipAction::Select(id) => {
                if let Some(key) = active {
                    self.set_tab_conn(key, Some(id));
                }
            }
            ChipAction::New => self.open_new_connection(),
            ChipAction::Edit => {
                if let Some(id) = current {
                    self.open_edit_connection(&id);
                }
            }
            ChipAction::Reconnect => {
                if let Some(id) = current {
                    self.request_reconnect(&id);
                }
            }
            ChipAction::Disconnect => {
                if let Some(id) = current {
                    self.request_disconnect(&id);
                }
            }
        }
    }

    /// Run, run script and stop, then the transaction controls (inline, or folded into an
    /// overflow button when the bar is narrow).
    fn session_controls(&mut self, ui: &mut egui::Ui, running: bool, tx_inline: bool, tx_id: Id) {
        let palette = theme::palette(ui.ctx());
        icons::toolbar_divider(ui);
        let run = icons::toolbar_button(ui, true, icons::RUN, Some(palette.success), false, "Run");
        let run = theme::tooltip_for(
            run,
            "Run statement",
            ActionId::RunStatement,
            Some("The statement at the caret, or the selection"),
        );
        if run.clicked() {
            self.run_active();
        }
        let script = icons::toolbar_button(ui, true, icons::RUN_SCRIPT, None, false, "Run script");
        let script = theme::tooltip_for(
            script,
            "Run script",
            ActionId::RunScript,
            Some("Every statement in the editor, one after another"),
        );
        if script.clicked() {
            self.run_script_active();
        }
        let stop =
            icons::toolbar_button(ui, running, icons::STOP, Some(palette.error), false, "Stop");
        let stop = theme::tooltip_for(stop, "Stop the running query", ActionId::Stop, None);
        if stop.clicked() {
            self.stop_active();
        }

        icons::toolbar_divider(ui);
        if tx_inline {
            group(ui, tx_id, |ui| self.transaction_controls(ui, false));
        } else {
            let more = icons::toolbar_button(ui, true, icons::MORE, None, false, "More tools");
            let more = theme::tooltip(more, "Transaction controls", None, None);
            egui::Popup::menu(&more)
                .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = Spacing::S;
                        self.transaction_controls(ui, true);
                    });
                });
        }
    }

    pub(crate) fn toolbar(&mut self, ui: &mut egui::Ui) {
        let operation_count = self
            .engine
            .operations()
            .iter()
            .filter(|o| {
                matches!(
                    o.state,
                    sqlr_engine::OperationState::Running | sqlr_engine::OperationState::Cancelling
                )
            })
            .count();
        let active = self.active_key();
        let running = active
            .and_then(|key| tab_mut(&mut self.dock, key))
            .is_some_and(|t| t.is_running(&self.engine));
        let palette = theme::palette(ui.ctx());
        let native = self.native_menu_installed();
        let button = icons::TOOLBAR_SIZE.x;

        // The space the left part may use: the right part (five buttons and a divider) is
        // fixed. Then the groups are budgeted by priority: the chip at its narrowest, the
        // transaction controls, the full menus, and what is left widens the chip.
        let right_width =
            5.0 * button + 6.0 * Spacing::XS + 1.0 + if operation_count > 0 { 140.0 } else { 0.0 };
        let gap = Spacing::S;
        let divider = 1.0 + gap;
        let menus_id = Id::new("toolbar_menus_width");
        let tx_id = Id::new("toolbar_tx_width");
        // The run and transaction controls act on the active tab's session: without a tab
        // on a connection there is nothing for them to do, so the bar leaves them out.
        let current = self.active_tab().and_then(|t| t.conn.clone());
        let session = current.is_some();
        let fixed = 24.0 + gap // app mark
            + divider
            + if session { 3.0 * (button + gap) + divider } else { 0.0 } // run, script, stop
            + if native { 0.0 } else { button + gap } // the compact menu, at least
            + divider
            + Spacing::M;
        let mut spare = ui.available_width() - right_width - fixed - CHIP_MIN;
        let tx_width = measured(ui, tx_id, TX_ESTIMATE);
        let tx_inline = session && spare >= tx_width + divider;
        if tx_inline {
            spare -= tx_width + divider;
        } else if session {
            spare -= button + gap; // the overflow button
        }
        let menus_extra = measured(ui, menus_id, MENUS_ESTIMATE) - button;
        let menus_full = !native && spare >= menus_extra;
        if menus_full {
            spare -= menus_extra;
        }
        let chip_width = (CHIP_MIN + spare.max(0.0)).min(CHIP_MAX);

        let layout = Layout::left_to_right(Align::Center);
        ui.with_layout(layout, |ui| {
            ui.spacing_mut().item_spacing.x = gap;

            // App mark and the classic menus, kept compact so the bar reads as one row.
            // With the macOS menu bar installed the menus live there instead.
            icons::app_mark(ui, "sqlr");
            if menus_full {
                ui.scope(|ui| {
                    ui.spacing_mut().button_padding = egui::vec2(6.0, 3.0);
                    ui.style_mut()
                        .text_styles
                        .insert(TextStyle::Button, FontId::proportional(12.5));
                    group(ui, menus_id, |ui| self.menu(ui));
                });
            } else if !native {
                let menu = icons::toolbar_button(ui, true, icons::MENU, None, false, "Menu");
                let menu =
                    theme::tooltip(menu, "Menu", None, Some("File, Edit, View, Query, Help"));
                egui::Popup::menu(&menu)
                    .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
                    .style(theme::menu_style(ui.ctx()))
                    .info(egui::UiStackInfo::new(egui::UiKind::Menu).with_tag_value(
                        egui::containers::menu::MenuConfig::MENU_CONFIG_TAG,
                        theme::menu_config(ui.ctx()),
                    ))
                    .show(|ui| self.menu(ui));
            }
            icons::toolbar_divider(ui);

            // The tab's connection, tinted with its own color (profile color as fallback).
            let chip = current
                .as_ref()
                .and_then(|id| self.connection(id))
                .map(|c| self.chip_item(c));
            // A SQL tab lists only SQL connections: document and key-value ones open their
            // own tabs.
            let nosql_tab = self.active_is_nosql_tab();
            let action = ui
                .add_enabled_ui(active.is_some(), |ui| {
                    ui.scope(|ui| {
                        ui.set_max_width(chip_width);
                        // The list is built only while the popup is open.
                        connection_chip(ui, chip.as_ref(), || {
                            self.picker_conn_names(nosql_tab)
                                .iter()
                                .filter_map(|(id, _)| self.connection(id))
                                .map(|c| self.chip_item(c))
                                .collect()
                        })
                    })
                    .inner
                })
                .inner;
            if let Some(action) = action {
                self.chip_action(action, active, current);
            }

            if session {
                self.session_controls(ui, running, tx_inline, tx_id);
            }

            // The workspace toggles sit on the far right.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = Spacing::XS;
                if operation_count > 0 {
                    if ui
                        .button(format!("Operations ({operation_count})"))
                        .clicked()
                    {
                        self.show_operations = true;
                    }
                    ui.spinner();
                }

                let settings =
                    icons::toolbar_button(ui, true, icons::SETTINGS, None, false, "Settings");
                if theme::tooltip_for(settings, "Settings", ActionId::Settings, None).clicked() {
                    self.open_preferences(sqlr_core::Level::Global);
                }
                // The quick toggle for the loopback MCP server, next to the workspace toggles.
                let mcp_on = self.settings.agents_enabled;
                let mcp_color = match self.mcp.status {
                    McpStatus::Running => Some(palette.success),
                    McpStatus::Starting => Some(palette.warning),
                    McpStatus::PortBusy | McpStatus::KeychainError | McpStatus::Error => {
                        Some(palette.error)
                    }
                    McpStatus::Stopped => None,
                };
                let mcp_title = if mcp_on {
                    format!("Agent access (MCP): {}", self.mcp.status.label())
                } else {
                    "Agent access (MCP): off".to_string()
                };
                let mcp_detail = if mcp_on {
                    "AI assistants on this computer can query the connections you chose. \
                     Click to turn it off."
                } else {
                    "Let AI assistants on this computer query the connections you choose. \
                     Click to turn it on."
                };
                let mcp = icons::toolbar_button(
                    ui,
                    true,
                    icons::MCP,
                    mcp_color,
                    mcp_on,
                    "Toggle agent access (MCP)",
                );
                if theme::tooltip(mcp, &mcp_title, None, Some(mcp_detail)).clicked() {
                    self.set_agents_enabled(!mcp_on, ui.ctx());
                }
                let value = icons::toolbar_button(
                    ui,
                    true,
                    icons::PANEL,
                    None,
                    self.show_value_panel,
                    "Toggle value panel",
                );
                if theme::tooltip_for(
                    value,
                    "Value panel",
                    ActionId::ToggleValuePanel,
                    Some("The full value of the selected cell, for long text and JSON"),
                )
                .clicked()
                {
                    self.show_value_panel = !self.show_value_panel;
                }
                let history = icons::toolbar_button(
                    ui,
                    true,
                    icons::HISTORY,
                    None,
                    self.show_history,
                    "Toggle history panel",
                );
                if theme::tooltip_for(
                    history,
                    "Query history",
                    ActionId::ToggleHistory,
                    Some("Every query you ran, searchable"),
                )
                .clicked()
                {
                    self.show_history = !self.show_history;
                }
                let terminal_open = self.terminal.is_open();
                if ui.available_width() > 210.0 {
                    let terminal = icons::toolbar_button(
                        ui,
                        true,
                        icons::TERMINAL,
                        None,
                        terminal_open,
                        "Toggle terminal",
                    );
                    if theme::tooltip_for(
                        terminal,
                        "Terminal",
                        ActionId::ToggleTerminal,
                        Some("A shell in the bottom panel"),
                    )
                    .clicked()
                    {
                        self.terminal.toggle();
                    }
                }
                icons::toolbar_divider(ui);
                let search =
                    icons::toolbar_button(ui, true, icons::SEARCH, None, false, "Command palette");
                let go_to = match sqlr_ui::keymap::shortcut(ui.ctx(), ActionId::OpenObject) {
                    Some(keys) => {
                        format!(
                            "Commands, snippets and history. Go to a table or view with {keys}."
                        )
                    }
                    None => "Commands, snippets and history.".to_string(),
                };
                if theme::tooltip_for(search, "Command palette", ActionId::Commands, Some(&go_to))
                    .clicked()
                {
                    self.open_palette(PaletteMode::Commands);
                }
            });
        });

        // A hairline closes the bar against the tab strip.
        let rect = ui.max_rect();
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, palette.border),
        );
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use sqlr_core::{ActionId, Chord};

    use crate::SqlrApp;
    use crate::palette::tests::bare_app;

    fn harness(app: SqlrApp) -> Harness<'static, SqlrApp> {
        let mut h = Harness::builder()
            .with_size((1400.0, 600.0))
            .with_os(egui::os::OperatingSystem::Mac)
            .build_ui_state(|ui, app: &mut SqlrApp| app.ui(ui), app);
        sqlr_ui::theme::install_fonts(&h.ctx, &[]);
        h.run_steps(3);
        h
    }

    fn hover(h: &mut Harness<'static, SqlrApp>, label: &str) {
        h.get_by_label(label).hover();
        // Past egui's tooltip delay.
        for _ in 0..60 {
            h.step();
        }
    }

    #[test]
    fn toolbar_tooltips_name_the_command_and_its_shortcut() {
        let (mut app, _dir) = bare_app();
        app.keymap
            .keymap
            .set_chords(ActionId::RunScript, vec![Chord::parse("F6").unwrap()]);
        let mut h = harness(app);
        // The palettes' keys are on screen: the status bar and the welcome page.
        h.get_by_label("⇧⌘P  Commands");
        h.get_by_label("Go to a table");
        h.get_by_label("⌘P");
        with_tab_on_a_connection(h.state_mut());
        h.run_steps(3);
        hover(&mut h, "Settings");
        h.get_by_label("⌘,");
        hover(&mut h, "Toggle terminal");
        h.get_by_label("⌃`");
        h.get_by_label("A shell in the bottom panel");
        hover(&mut h, "Toggle agent access (MCP)");
        h.get_by_label_contains("Let AI assistants on this computer query");
        // Disabled buttons still explain themselves, with the user's own keys.
        hover(&mut h, "Run script");
        h.get_by_label("F6");
    }

    #[test]
    fn menus_offer_keyboard_only_actions_with_their_shortcuts() {
        let (app, _dir) = bare_app();
        let mut h = harness(app);
        h.get_by_label("Query").click();
        h.run_steps(3);
        for label in ["Explain analyze", "Format SQL"] {
            h.get_by_label(label);
        }
        // With no tab on a connection the toolbar has no Commit or Rollback of its own.
        for label in ["Commit", "Rollback"] {
            assert_eq!(h.get_all_by_label(label).count(), 1, "{label}");
        }
    }

    /// `app` with a saved connection "c1" and a query tab on it.
    fn with_tab_on_a_connection(app: &mut SqlrApp) {
        app.connections.push(sqlr_core::ConnectionConfig {
            id: "c1".into(),
            name: "db".into(),
            driver: "fake".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: false,
            agent_access: Default::default(),
        });
        app.new_tab(None, Some("c1".into()), "select 1");
    }

    #[test]
    fn session_controls_appear_only_for_a_tab_on_a_connection() {
        let (app, _dir) = bare_app();
        let mut h = harness(app);
        for label in ["Run", "Run script", "Stop", "Read-only"] {
            assert!(
                h.query_by_label(label).is_none(),
                "{label} without a session"
            );
        }
        with_tab_on_a_connection(h.state_mut());
        h.run_steps(3);
        for label in ["Run", "Run script", "Stop", "Read-only"] {
            assert!(h.query_by_label(label).is_some(), "{label} on a connection");
        }
        // The explorer has its own refresh; the toolbar no longer repeats it.
        assert!(h.query_by_label("Refresh").is_none());
    }

    #[test]
    fn the_tools_menu_lists_the_data_tools() {
        let (app, _dir) = bare_app();
        let mut h = harness(app);
        h.get_by_label("Tools").click();
        h.run_steps(3);
        for label in [
            "Transfer data…",
            "Compare schemas…",
            "Export result…",
            "Drivers…",
        ] {
            h.get_by_label(label);
        }
    }

    #[test]
    fn the_status_bar_toggles_the_terminal_and_hides_editor_segments_without_an_editor() {
        let (app, _dir) = bare_app();
        let mut h = harness(app);
        assert!(h.query_by_label("UTF-8").is_none());
        h.get_by_label("Terminal").click();
        h.run_steps(2);
        assert!(h.state().terminal.is_open());
        h.state_mut().terminal.toggle();
        with_tab_on_a_connection(h.state_mut());
        h.run_steps(3);
        h.get_by_label("UTF-8");
        h.get_by_label("Ln 1, Col 1");
    }
}
