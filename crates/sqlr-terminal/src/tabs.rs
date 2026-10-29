//! Session-only terminal tabs with one independent panel and PTY per tab.

use std::sync::Arc;
use std::thread::JoinHandle;

use crate::{SessionConfig, TerminalAction, TerminalPanel, TerminalSession, TerminalTheme};

/// A tab label in the terminal header.
pub(crate) struct TabView {
    pub id: u64,
    pub selected: bool,
    pub name: String,
}

struct Tab {
    id: u64,
    panel: TerminalPanel,
    name: Option<String>,
    shell_only: bool,
}

/// The terminal dock. Hiding it keeps all shells; closing a tab ends only its shell.
#[derive(Default)]
pub struct TerminalTabs {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    open: bool,
    config: SessionConfig,
    agent_name: Option<String>,
    reveal_active: bool,
    closing: Vec<JoinHandle<()>>,
}

impl TerminalTabs {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn focused(&self) -> bool {
        self.open
            && self
                .tabs
                .get(self.active)
                .is_some_and(|t| t.panel.focused())
    }

    pub fn started(&self) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|t| t.panel.started())
    }

    pub fn session(&self) -> Option<&TerminalSession> {
        self.tabs.get(self.active)?.panel.session()
    }

    pub fn canvas_rect(&self) -> egui::Rect {
        self.tabs
            .get(self.active)
            .map_or(egui::Rect::NOTHING, |t| t.panel.canvas_rect())
    }

    pub fn config(&self) -> &SessionConfig {
        self.tabs
            .get(self.active)
            .map_or(&self.config, |tab| tab.panel.config())
    }

    pub fn set_config(&mut self, config: SessionConfig) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let mut tab_config = config.clone();
            if tab.shell_only {
                tab_config.shell.clear();
            }
            tab.panel.set_config(tab_config);
        }
        self.config = config;
    }

    pub fn set_agent_name(&mut self, name: Option<String>) {
        if let Some(tab) = self.tabs.get_mut(self.active)
            && !tab.shell_only
            && tab.name.is_none()
        {
            tab.name = name.clone();
        }
        self.agent_name = name;
    }

    pub fn open(&mut self) {
        self.open = true;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.panel.open();
        } else {
            self.new_tab();
        }
    }

    /// Hides the dock while its processes keep running.
    pub fn close(&mut self) {
        self.open = false;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.panel.close();
        }
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    /// Opens a fresh shell using the current launch preferences.
    pub fn new_tab(&mut self) {
        self.create_tab(false);
    }

    /// Opens the platform login shell independently of the configured agent.
    pub fn new_shell_tab(&mut self) {
        self.create_tab(true);
    }

    fn create_tab(&mut self, shell_only: bool) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.panel.close();
        }
        self.next_id += 1;
        self.reveal_active = true;
        let mut panel = TerminalPanel::default();
        let mut config = self.config.clone();
        if shell_only {
            config.shell.clear();
        }
        panel.set_config(config);
        panel.open();
        self.active = self.tabs.len();
        self.tabs.push(Tab {
            id: self.next_id,
            panel,
            name: if shell_only {
                None
            } else {
                self.agent_name.clone()
            },
            shell_only,
        });
        self.open = true;
    }

    pub fn select(&mut self, id: u64) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.panel.close();
        }
        self.active = index;
        self.reveal_active = true;
        self.open();
    }

    /// Removes a tab and reaps its PTY on a worker, keeping the UI responsive.
    pub fn close_tab(&mut self, id: u64) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        self.reveal_active = index == self.active;
        let mut tab = self.tabs.remove(index);
        self.closing.retain(|worker| !worker.is_finished());
        self.closing.push(std::thread::spawn(move || {
            tab.panel.kill();
            drop(tab);
        }));
        if self.tabs.is_empty() {
            self.active = 0;
            self.open = false;
        } else {
            if index < self.active {
                self.active -= 1;
            }
            self.active = self.active.min(self.tabs.len() - 1);
            if self.open {
                self.tabs[self.active].panel.open();
            }
        }
    }

    /// Closes tabs preceding the reference, retaining the selected surviving session.
    pub fn close_tabs_left(&mut self, id: u64) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        for index in (0..index).rev() {
            self.close_tab(self.tabs[index].id);
        }
        self.reveal_active = true;
    }

    /// Closes tabs following the reference, retaining the selected surviving session.
    pub fn close_tabs_right(&mut self, id: u64) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        for index in (index + 1..self.tabs.len()).rev() {
            self.close_tab(self.tabs[index].id);
        }
        self.reveal_active = true;
    }

    pub fn restart(&mut self, repaint: Arc<dyn Fn() + Send + Sync>) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.name = if tab.shell_only {
                None
            } else {
                self.agent_name.clone()
            };
            tab.panel.restart(repaint);
        }
    }

    /// Stops every shell on application quit, including those in hidden tabs.
    pub fn kill(&mut self) {
        for tab in &mut self.tabs {
            tab.panel.kill();
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        theme: &TerminalTheme,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Vec<TerminalAction> {
        for tab in &mut self.tabs {
            if tab.name.is_none() {
                tab.name = tab
                    .panel
                    .session()
                    .map(|session| session.program_name().to_owned());
            }
        }
        let views: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| TabView {
                id: tab.id,
                selected: index == self.active,
                name: tab.name.clone().unwrap_or_else(|| "Terminal".into()),
            })
            .collect();
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return Vec::new();
        };
        let reveal_active = std::mem::take(&mut self.reveal_active);
        tab.panel.show_tabs(
            ui,
            theme,
            repaint,
            &views,
            reveal_active,
            self.agent_name.is_some(),
        )
    }
}

impl Drop for TerminalTabs {
    fn drop(&mut self) {
        self.kill();
        for worker in self.closing.drain(..) {
            let _ = worker.join();
        }
    }
}
