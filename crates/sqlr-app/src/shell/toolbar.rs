//! The main toolbar: app mark and menus, the connection chip, the execution and transaction
//! controls, then the workspace toggles on the right. In a narrow window the menus fold into
//! one button and the transaction controls into an overflow button, so nothing overlaps or
//! leaves the window.

use egui::{
    Align, Button, FontId, Id, KeyboardShortcut, Layout, PopupCloseBehavior, Stroke, TextStyle,
};
use sqlr_core::ConnectionConfig;
use sqlr_engine::SessionStatus;
use sqlr_ui::theme::{self, Spacing, icons};
use sqlr_ui::{ChipAction, ChipItem, ChipState, PaletteMode, connection_chip};

use crate::agents::McpStatus;
use crate::app::{
    CLOSE_TAB, COPY, CUT, NEW_TAB, PASTE, REDO, RUN, RUN_SCRIPT, SELECT_ALL, SETTINGS, SqlrApp,
    UNDO, tab_mut,
};
use crate::palette::AppCommand;

/// Widths the toolbar budgets before it has measured its groups, in points.
const MENUS_ESTIMATE: f32 = 220.0;
const TX_ESTIMATE: f32 = 300.0;
/// The chip's narrowest and widest sizes in the toolbar.
const CHIP_MIN: f32 = 96.0;
const CHIP_MAX: f32 = 220.0;

/// One row of the in-window menus: a label, an optional shortcut hint and the command it
/// runs. The same [`AppCommand`] dispatcher serves the native menu, so the two menus cannot
/// drift apart.
fn command_item(
    ui: &mut egui::Ui,
    app: &mut SqlrApp,
    label: &str,
    _shortcut: Option<KeyboardShortcut>,
    command: AppCommand,
) {
    let button = Button::new(label);
    let button = if let Some(c) = command
        .action_id()
        .and_then(|a| app.keymap.keymap.chord_for(a))
    {
        button.shortcut_text(c.label(cfg!(target_os = "macos")))
    } else {
        button
    };
    if ui.add(button).clicked() {
        app.run_command(command, ui.ctx());
    }
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
        ui.menu_button("File", |ui| {
            command_item(ui, self, "New query tab", Some(NEW_TAB), AppCommand::NewTab);
            command_item(ui, self, "Close tab", Some(CLOSE_TAB), AppCommand::CloseTab);
            ui.separator();
            command_item(ui, self, "New connection", None, AppCommand::NewConnection);
            command_item(ui, self, "Connection profiles…", None, AppCommand::Profiles);
            ui.separator();
            command_item(ui, self, "Import…", None, AppCommand::ImportConnections);
            command_item(ui, self, "Export…", None, AppCommand::ExportConnections);
            ui.separator();
            command_item(ui, self, "Settings…", Some(SETTINGS), AppCommand::Settings);
            ui.separator();
            command_item(ui, self, "Operations", None, AppCommand::ShowOperations);
            command_item(ui, self, "Quit", None, AppCommand::Quit);
        });
        ui.menu_button("Edit", |ui| {
            command_item(ui, self, "Undo", Some(UNDO), AppCommand::Undo);
            command_item(ui, self, "Redo", Some(REDO), AppCommand::Redo);
            ui.separator();
            command_item(ui, self, "Cut", Some(CUT), AppCommand::Cut);
            command_item(ui, self, "Copy", Some(COPY), AppCommand::Copy);
            command_item(ui, self, "Paste", Some(PASTE), AppCommand::Paste);
            command_item(ui, self, "Delete", None, AppCommand::Delete);
            ui.separator();
            command_item(
                ui,
                self,
                "Select All",
                Some(SELECT_ALL),
                AppCommand::SelectAll,
            );
        });
        ui.menu_button("View", |ui| {
            ui.checkbox(&mut self.show_value_panel, "Value panel");
            ui.checkbox(&mut self.show_history, "History");
            let mut terminal = self.terminal.is_open();
            if ui.checkbox(&mut terminal, "Terminal").changed() {
                self.terminal.toggle();
            }
            let mut soft_wrap = self.settings.soft_wrap;
            if ui.checkbox(&mut soft_wrap, "Soft wrap").changed() {
                self.toggle_soft_wrap(ui.ctx());
            }
        });
        ui.menu_button("Query", |ui| {
            command_item(ui, self, "Run statement", Some(RUN), AppCommand::Run);
            command_item(
                ui,
                self,
                "Run script",
                Some(RUN_SCRIPT),
                AppCommand::RunScript,
            );
            command_item(ui, self, "Stop", None, AppCommand::Stop);
            ui.separator();
            command_item(ui, self, "Explain", None, AppCommand::Explain);
            command_item(
                ui,
                self,
                "Explain Analyze",
                None,
                AppCommand::ExplainAnalyze,
            );
            ui.separator();
            command_item(ui, self, "Save as snippet…", None, AppCommand::SaveSnippet);
        });
        ui.menu_button("Help", |ui| {
            command_item(ui, self, "Keyboard Shortcuts", None, AppCommand::Shortcuts);
            ui.separator();
            command_item(ui, self, "Diagnostics…", None, AppCommand::Diagnostics);
            command_item(
                ui,
                self,
                "Report a Problem…",
                None,
                AppCommand::ReportProblem,
            );
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
        let fixed = 24.0 + gap // app mark
            + divider
            + 3.0 * (button + gap) + divider // run, run script, stop
            + button + gap // refresh
            + if native { 0.0 } else { button + gap } // the compact menu, at least
            + divider
            + Spacing::M;
        let mut spare = ui.available_width() - right_width - fixed - CHIP_MIN;
        let tx_width = measured(ui, tx_id, TX_ESTIMATE);
        let tx_inline = spare >= tx_width + divider;
        if tx_inline {
            spare -= tx_width + divider;
        } else {
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
                let menu = icons::toolbar_button(ui, true, icons::MENU, None, false, "Menu")
                    .on_hover_text("Menu");
                egui::Popup::menu(&menu)
                    .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| self.menu(ui));
            }
            icons::toolbar_divider(ui);

            // The tab's connection, tinted with its own color (profile color as fallback).
            let current = self.active_tab().and_then(|t| t.conn.clone());
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

            icons::toolbar_divider(ui);
            // Execution.
            let can_run = active.is_some();
            let run =
                icons::toolbar_button(ui, can_run, icons::RUN, Some(palette.success), false, "Run")
                    .on_hover_text(format!(
                        "Run ({})",
                        sqlr_ui::keymap::ui_keymap(ui.ctx())
                            .label(sqlr_core::ActionId::RunStatement)
                            .unwrap_or_default()
                    ));
            if run.clicked() {
                self.run_active();
            }
            let script =
                icons::toolbar_button(ui, can_run, icons::RUN_SCRIPT, None, false, "Run script")
                    .on_hover_text(format!(
                        "Run script ({})",
                        sqlr_ui::keymap::ui_keymap(ui.ctx())
                            .label(sqlr_core::ActionId::RunScript)
                            .unwrap_or_default()
                    ));
            if script.clicked() {
                self.run_script_active();
            }
            let stop =
                icons::toolbar_button(ui, running, icons::STOP, Some(palette.error), false, "Stop")
                    .on_hover_text("Stop the running query (Esc)");
            if stop.clicked() {
                self.stop_active();
            }

            icons::toolbar_divider(ui);
            if tx_inline {
                group(ui, tx_id, |ui| self.transaction_controls(ui, false));
            } else {
                let more = icons::toolbar_button(ui, true, icons::MORE, None, false, "More tools")
                    .on_hover_text("Transaction controls");
                egui::Popup::menu(&more)
                    .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = Spacing::S;
                            self.transaction_controls(ui, true);
                        });
                    });
            }

            // The explorer's refresh, tied to the selected node.
            let selection = self.tree.selected.clone();
            let refresh = icons::toolbar_button(
                ui,
                selection.is_some(),
                icons::REFRESH,
                None,
                false,
                "Refresh",
            )
            .on_hover_text("Reload the selected explorer node or connection");
            if refresh.clicked()
                && let Some((conn, path)) = selection
            {
                self.engine.refresh(&conn, path);
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

                if icons::toolbar_button(ui, true, icons::SETTINGS, None, false, "Settings")
                    .on_hover_text("Settings")
                    .clicked()
                {
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
                let mcp_tip = if mcp_on {
                    format!(
                        "Agent access (MCP): {} · click to disable",
                        self.mcp.status.label()
                    )
                } else {
                    "Agent access (MCP): off · click to enable".to_string()
                };
                if icons::toolbar_button(
                    ui,
                    true,
                    icons::MCP,
                    mcp_color,
                    mcp_on,
                    "Toggle agent access (MCP)",
                )
                .on_hover_text(mcp_tip)
                .clicked()
                {
                    self.set_agents_enabled(!mcp_on, ui.ctx());
                }
                if icons::toolbar_button(
                    ui,
                    true,
                    icons::PANEL,
                    None,
                    self.show_value_panel,
                    "Toggle value panel",
                )
                .on_hover_text("Show or hide the value panel")
                .clicked()
                {
                    self.show_value_panel = !self.show_value_panel;
                }
                if icons::toolbar_button(
                    ui,
                    true,
                    icons::HISTORY,
                    None,
                    self.show_history,
                    "Toggle history panel",
                )
                .on_hover_text("Show or hide the query history")
                .clicked()
                {
                    self.show_history = !self.show_history;
                }
                let terminal_open = self.terminal.is_open();
                if ui.available_width() > 210.0
                    && icons::toolbar_button(
                        ui,
                        true,
                        icons::QUERY,
                        None,
                        terminal_open,
                        "Toggle terminal",
                    )
                    .on_hover_text("Terminal (Ctrl+Backtick)")
                    .clicked()
                {
                    self.terminal.toggle();
                }
                icons::toolbar_divider(ui);
                if icons::toolbar_button(ui, true, icons::SEARCH, None, false, "Command palette")
                    .on_hover_text("Commands, history and snippets (⌘⇧P)")
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
