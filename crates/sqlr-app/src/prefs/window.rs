//! The preferences window: sections on the left, a search field across every setting, and
//! the level (global, profile or connection) the values are edited at. Changes apply live;
//! Cancel puts everything back.

use std::path::Path;

use egui::{
    Align, Color32, ComboBox, CornerRadius, Frame, Id, Layout, Margin, Stroke, WidgetInfo,
    WidgetType, vec2,
};
use sqlr_core::{AgentAccess, Level, SECTIONS, SettingDef, search_settings, setting_defs};
use sqlr_store::LayeredSettings;
use sqlr_ui::theme::{self, Spacing, icons};

use super::resolve::Scopes;
use super::sections::Editing;
use crate::agents::AgentAccessRow;
use crate::dialogs::nav_item;

/// Which connections Settings → Agents lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AccessFilter {
    #[default]
    All,
    Exposed,
    Hidden,
}

impl AccessFilter {
    fn matches(self, access: AgentAccess) -> bool {
        match self {
            Self::All => true,
            Self::Exposed => access.is_exposed(),
            Self::Hidden => !access.is_exposed(),
        }
    }
}

/// Edits the app's live settings; the app derives everything else from them each frame.
struct PendingSave {
    receiver: std::sync::mpsc::Receiver<Result<(), sqlr_store::StoreError>>,
    live: LayeredSettings,
    keymap: sqlr_store::StoredKeymap,
    close: bool,
    frame: u64,
}

pub struct PrefsWindow {
    saving: Option<PendingSave>,
    /// The settings when the window opened (or last Applied): Cancel restores them.
    original: LayeredSettings,
    keymap: Option<sqlr_store::StoredKeymap>,
    keymap_editor: sqlr_ui::keymap_editor::KeymapEditor,
    query: String,
    section: &'static str,
    scope: Level,
    error: Option<String>,
    /// The Settings → Agents search field and access filter.
    agent_query: String,
    agent_filter: AccessFilter,
    /// Exposure changes picked in Settings → Agents, applied by the app after the frame.
    pub access_changes: Vec<(String, AgentAccess)>,
    /// The search field takes the keyboard when the window opens.
    focus_search: bool,
    /// "Drivers…" was clicked; the app opens the driver manager.
    pub open_drivers: bool,
    pub open_databases: bool,
    /// "Open settings folder" was clicked; the app opens it with the OS.
    pub open_settings_folder: bool,
    /// Settings → Agents status, set by the app before showing.
    pub mcp: crate::agents::McpPrefs,
    pub copy_mcp_config: bool,
    pub rotate_token: bool,
    pub open_terminal: bool,
    /// "Check for updates now" was clicked; the app starts the check.
    pub check_updates: bool,
}

fn section_icon(section: &str) -> &'static str {
    match section {
        "Appearance" => icons::APPEARANCE,
        "Editor" => icons::QUERY,
        "Result grid" => icons::TABLE,
        "Data formats" => icons::COLUMN,
        "Connections" => icons::CONNECTION,
        "Agents" => icons::VIEW,
        "Terminal" => icons::TERMINAL,
        "Keymap" => icons::KEYBOARD,
        "Drivers" => icons::DATABASE,
        _ => icons::SETTINGS,
    }
}

impl PrefsWindow {
    pub fn new_for_section(live: &LayeredSettings, scope: Level, section: &'static str) -> Self {
        let mut window = Self::new(live, scope);
        window.section = section;
        window.focus_search = false;
        window
    }

    pub fn new(live: &LayeredSettings, scope: Level) -> Self {
        Self {
            saving: None,
            original: live.clone(),
            keymap: None,
            keymap_editor: Default::default(),
            query: String::new(),
            // Overrides start where the per-connection settings are.
            section: SECTIONS[0],
            scope,
            error: None,
            agent_query: String::new(),
            agent_filter: AccessFilter::All,
            access_changes: Vec::new(),
            focus_search: true,
            open_drivers: false,
            open_databases: false,
            open_settings_folder: false,
            mcp: Default::default(),
            copy_mcp_config: false,
            rotate_token: false,
            open_terminal: false,
            check_updates: false,
        }
    }

    /// Returns `false` once the window should close. Apply and Save write the files; Save and
    /// Cancel close, and Cancel restores what the window opened with.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        live: &mut LayeredSettings,
        scopes: &Scopes,
        agent_rows: &[AgentAccessRow],
        config_dir: &Path,
        keymap: &mut sqlr_store::StoredKeymap,
    ) -> bool {
        if self.keymap.is_none() {
            self.keymap = Some(keymap.clone());
        }
        if let Some(pending) = &self.saving {
            ctx.request_repaint_after(std::time::Duration::from_millis(20));
            if ctx.cumulative_frame_nr() > pending.frame {
                let result = match pending.receiver.try_recv() {
                    Ok(result) => Some(result.map_err(|error| error.to_string())),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Some(Err("Preferences writer stopped".to_string()))
                    }
                    Err(_) => None,
                };
                if let Some(result) = result
                    && let Some(pending) = self.saving.take()
                {
                    match result {
                        Ok(()) => {
                            self.original = pending.live;
                            self.keymap = Some(pending.keymap);
                            self.error = None;
                            if pending.close {
                                return false;
                            }
                        }
                        Err(error) => self.error = Some(format!("Could not save: {error}")),
                    }
                }
            }
        }
        let p = theme::palette(ctx);
        let saving = self.saving.is_some();
        let changed = *live != self.original
            || self
                .keymap
                .as_ref()
                .is_some_and(|original| original != keymap);
        let response = theme::Dialog::new("preferences", icons::SETTINGS, "Preferences")
            .subtitle("Application, editor and per-connection preferences")
            .width(880.0)
            .body_height(600.0)
            .min_body_height(260.0)
            .body_margin(Margin::ZERO)
            .show(ctx, |d| {
                d.body(|ui| {
                    if saving {
                        ui.disable();
                    }
                    egui::Panel::left(Id::new("prefs_sections"))
                        .resizable(false)
                        .exact_size(196.0)
                        .frame(
                            Frame::new()
                                .fill(p.bg_subtle)
                                .inner_margin(Margin::symmetric(12, 14)),
                        )
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = Spacing::XS;
                            let search = egui::TextEdit::singleline(&mut self.query)
                                .hint_text("Search settings")
                                .desired_width(f32::INFINITY);
                            let search = sqlr_ui::text_edit(ui, search);
                            if std::mem::take(&mut self.focus_search) {
                                search.request_focus();
                            }
                            ui.add_space(Spacing::M);
                            for section in SECTIONS {
                                let active =
                                    self.query.trim().is_empty() && self.section == *section;
                                let row = nav_item(ui, section_icon(section), section, active);
                                let key = scopes.resolve_key(&self.scope);
                                let dirty = setting_defs().iter().any(|def| {
                                    def.section == *section
                                        && self.original.get(def.id, key).0
                                            != live.get(def.id, key).0
                                }) || (*section == "Keymap"
                                    && self
                                        .keymap
                                        .as_ref()
                                        .is_some_and(|original| original != keymap));
                                if dirty {
                                    ui.painter().circle_filled(
                                        egui::pos2(row.rect.right() - 8.0, row.rect.center().y),
                                        3.0,
                                        p.accent,
                                    );
                                }
                                if row.clicked() {
                                    self.section = section;
                                    self.query.clear();
                                }
                            }
                        });

                    egui::CentralPanel::no_frame().show(ui, |ui| {
                        Frame::new()
                            .fill(p.bg_raised)
                            .inner_margin(Margin::symmetric(28, 22))
                            .show(ui, |ui| {
                                let height = ui.available_height();
                                theme::scroll_body(ui, "prefs_page", height, height, |ui| {
                                    if self.section == "Keymap" && self.query.trim().is_empty() {
                                        self.keymap_editor.show(
                                            ui,
                                            &mut keymap.keymap,
                                            &mut keymap.preset,
                                        );
                                    } else {
                                        self.page(ui, live, scopes, agent_rows);
                                    }
                                });
                            });
                    });
                });
                d.footer(|f| {
                    let blocked = if saving {
                        Some("Saving preferences…")
                    } else if !changed {
                        Some("No unsaved changes")
                    } else {
                        None
                    };
                    let save = f.primary("Save", blocked);
                    let apply = f.button("Apply", !saving && changed);
                    if (save || apply) && self.saving.is_none() {
                        self.persist_all(live, config_dir, keymap, save, ctx);
                    }
                    f.cancel();
                    if f.secondary("Open settings folder", true) {
                        self.open_settings_folder = true;
                    }
                    if changed {
                        f.left().label("Unsaved changes");
                    }
                    // After the buttons, so a save started this frame says so at once.
                    if let Some(error) = &self.error {
                        f.left().colored_label(p.error, error);
                    } else if self.saving.is_some() {
                        f.left().label("Saving preferences…");
                    }
                });
            });
        if response.closed && self.saving.is_none() {
            *live = self.original.clone();
            if let Some(original) = &self.keymap {
                *keymap = original.clone();
            }
            return false;
        }
        true
    }

    fn page(
        &mut self,
        ui: &mut egui::Ui,
        live: &mut LayeredSettings,
        scopes: &Scopes,
        agent_rows: &[AgentAccessRow],
    ) {
        if self.section == "Connections"
            || setting_defs()
                .iter()
                .any(|d| d.section == self.section && d.levels == sqlr_core::Levels::All)
        {
            self.scope_picker(ui, scopes);
            ui.add_space(Spacing::L);
        }
        let searching = !self.query.trim().is_empty();
        if searching {
            let found = search_settings(&self.query);
            let mut editing = Editing {
                live,
                scope: &self.scope,
                scopes,
                error: &mut self.error,
            };
            let mut any = false;
            for section in SECTIONS {
                let defs: Vec<&'static SettingDef> = found
                    .iter()
                    .copied()
                    .filter(|d| d.section == *section && editing.applies(d))
                    .collect();
                if defs.is_empty() {
                    continue;
                }
                any = true;
                theme::section_title(ui, section);
                editing.rows(ui, &defs);
                ui.add_space(Spacing::M);
            }
            if !any {
                theme::hint(ui, "No setting matches the search.");
            }
            return;
        }
        theme::section_title(ui, self.section);
        ui.add_space(Spacing::M);
        let defs: Vec<&'static SettingDef> = setting_defs()
            .iter()
            .filter(|d| d.section == self.section)
            .collect();
        // The server's configuration comes first; both blocks are on the same page.
        if self.section == "Agents" {
            if self.scope == Level::Global {
                theme::hint(
                    ui,
                    "Let AI assistants on this computer query the connections you choose.",
                );
                ui.add_space(Spacing::M);
                let mut editing = Editing {
                    live,
                    scope: &self.scope,
                    scopes,
                    error: &mut self.error,
                };
                editing.rows(ui, &defs);
                self.agents_actions(ui);
                ui.add_space(Spacing::XL);
                theme::hairline(ui);
                ui.add_space(Spacing::L);
            }
            self.agents_access(ui, agent_rows);
            return;
        }
        let mut editing = Editing {
            live,
            scope: &self.scope,
            scopes,
            error: &mut self.error,
        };
        editing.rows(ui, &defs);
        if self.scope == Level::Global {
            self.section_actions(ui);
            self.agents_actions(ui);
        }
    }

    /// "Applies to": the level the values below are edited at.
    fn scope_picker(&mut self, ui: &mut egui::Ui, scopes: &Scopes) {
        ui.horizontal(|ui| {
            ui.label("Applies to");
            ComboBox::from_id_salt("prefs_scope")
                .selected_text(scopes.name(&self.scope))
                .width(260.0)
                .show_ui(ui, |ui| {
                    for level in scopes.levels() {
                        ui.selectable_value(&mut self.scope, level.clone(), scopes.name(&level));
                    }
                });
            if self.scope != Level::Global {
                theme::hint(
                    ui,
                    "Only settings that can differ per connection are listed.",
                );
            }
        });
    }

    /// Settings → Agents' status and actions.
    fn agents_actions(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        if self.section == "Agents" {
            ui.add_space(Spacing::M);
            theme::hairline(ui);
            ui.add_space(Spacing::S);
            ui.horizontal(|ui| {
                let color = match self.mcp.status {
                    crate::agents::McpStatus::Running => p.success,
                    crate::agents::McpStatus::Starting => p.warning,
                    crate::agents::McpStatus::Stopped => p.text_disabled,
                    _ => p.error,
                };
                ui.label(
                    egui::RichText::new(self.mcp.status.label())
                        .strong()
                        .color(color),
                );
                if let Some(url) = &self.mcp.url {
                    ui.label(egui::RichText::new(url).monospace().small());
                    if ui.small_button("Copy endpoint").clicked() {
                        ui.ctx().copy_text(url.clone());
                    }
                    ui.label(
                        egui::RichText::new(format!("{} active session(s)", self.mcp.sessions))
                            .small()
                            .weak(),
                    );
                }
            });
            if self.mcp.token_present {
                ui.label(
                    egui::RichText::new("A bearer token is saved securely on this device.")
                        .small()
                        .weak(),
                );
            }
            ui.add_space(Spacing::S);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        self.mcp.status.is_running(),
                        egui::Button::new("Copy MCP config"),
                    )
                    .on_hover_text("Contains the bearer token; paste it into the agent")
                    .on_disabled_hover_text(
                        "Enable the local agent server and wait until it is running",
                    )
                    .clicked()
                {
                    self.copy_mcp_config = true;
                }
                if ui
                    .add_enabled(
                        self.mcp.status.is_running(),
                        egui::Button::new("Rotate token"),
                    )
                    .on_hover_text("Revokes the old token and disconnects active agents")
                    .on_disabled_hover_text(
                        "Enable the local agent server before rotating its token",
                    )
                    .clicked()
                {
                    self.rotate_token = true;
                }
            });
            ui.add_space(Spacing::XS);
            theme::hint(
                ui,
                "The server listens on 127.0.0.1 only. Enabling it exposes no connection: set agent access on each connection.",
            );
            if let Some(error) = &self.mcp.error {
                ui.add_space(Spacing::XS);
                ui.colored_label(p.error, error);
            }
        }
        if self.section == "Terminal" {
            ui.add_space(Spacing::M);
            theme::hairline(ui);
            ui.add_space(Spacing::S);
            if ui.button("Open terminal").clicked() {
                self.open_terminal = true;
            }
            ui.add_space(Spacing::XS);
            theme::hint(
                ui,
                "Changes apply on Restart. The shell command is never shown to agents or logged; do not embed credentials in it. Terminal output is never logged.",
            );
        }
    }

    /// Settings → Agents' connection list: every saved connection, grouped by folder, with
    /// the exposure level one click away.
    fn agents_access(&mut self, ui: &mut egui::Ui, rows: &[AgentAccessRow]) {
        let p = theme::palette(ui.ctx());
        // Keep a margin at the right: the header, the filters and the cards all share one
        // edge, clear of the scrollbar.
        let width = (ui.available_width() - Spacing::XL).max(240.0);
        ui.vertical(|ui| {
            ui.set_width(width);
            let exposed = rows.iter().filter(|row| row.access.is_exposed()).count();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Connection access").strong().size(14.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (fill, text) = if exposed > 0 {
                        (p.accent_soft, p.accent)
                    } else {
                        (p.bg_subtle, p.text_muted)
                    };
                    Frame::new()
                        .fill(fill)
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(format!("{exposed} of {} exposed", rows.len()))
                                    .size(11.0)
                                    .color(text),
                            );
                        });
                });
            });
            ui.add_space(Spacing::XS);
            theme::hint(
                ui,
                "Agents see only the connections exposed here. Changes apply immediately; the connection profile and its confirmations still apply.",
            );
            if !self.mcp.status.is_running() && self.mcp.status != crate::agents::McpStatus::Starting
            {
                ui.add_space(Spacing::XS);
                theme::hint(
                    ui,
                    "The MCP server is off. Exposure is saved and takes effect when you enable the server below.",
                );
            }

            if rows.is_empty() {
                ui.add_space(Spacing::M);
                theme::hint(
                    ui,
                    "No saved connections yet. Add one from Data Sources to expose it to agents.",
                );
                return;
            }

            ui.add_space(Spacing::M);
            let mut query = std::mem::take(&mut self.agent_query);
            let mut filter = self.agent_filter;
            ui.horizontal(|ui| {
                // The filters are anchored to the right edge so the search field and the
                // connection cards below share one right margin.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for (mode, label) in [
                        (AccessFilter::Hidden, "Hidden"),
                        (AccessFilter::Exposed, "Exposed"),
                        (AccessFilter::All, "All"),
                    ] {
                        if theme::segment(ui, filter == mode, label).clicked() {
                            filter = mode;
                        }
                    }
                    ui.add_space(Spacing::S);
                    let width = ui.available_width();
                    ui.allocate_ui_with_layout(
                        vec2(width, ui.spacing().interact_size.y),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_width(width);
                            let search = egui::TextEdit::singleline(&mut query)
                                .hint_text("Search connections")
                                .desired_width(width);
                            let response = sqlr_ui::text_edit(ui, search);
                            if response.has_focus() || !query.is_empty() {
                                response
                                    .on_hover_text("Filters by name, driver and folder");
                            }
                        },
                    );
                });
            });
            self.agent_query = query;
            self.agent_filter = filter;

            let needle = self.agent_query.trim().to_lowercase();
            let mut visible: Vec<&AgentAccessRow> = rows
                .iter()
                .filter(|row| filter.matches(row.access))
                .filter(|row| {
                    needle.is_empty()
                        || row.name.to_lowercase().contains(&needle)
                        || row.driver_name.to_lowercase().contains(&needle)
                        || row
                            .folder
                            .as_deref()
                            .is_some_and(|folder| folder.to_lowercase().contains(&needle))
                })
                .collect();
            visible.sort_by(|a, b| {
                (a.folder.as_deref().unwrap_or(""), &a.name)
                    .cmp(&(b.folder.as_deref().unwrap_or(""), &b.name))
            });

            if visible.is_empty() {
                ui.add_space(Spacing::M);
                theme::hint(ui, "No connection matches the filter.");
                return;
            }

            let mut groups: Vec<(Option<&str>, Vec<&AgentAccessRow>)> = Vec::new();
            for row in visible {
                match groups.last_mut() {
                    Some((folder, grouped)) if *folder == row.folder.as_deref() => {
                        grouped.push(row)
                    }
                    _ => groups.push((row.folder.as_deref(), vec![row])),
                }
            }
            // A lone top-level group needs no heading; a list with folders does.
            let show_ungrouped = groups.len() > 1 || groups.iter().any(|(folder, _)| folder.is_some());
            for (folder, grouped) in groups {
                if folder.is_some() || show_ungrouped {
                    agents_folder_header(ui, folder, grouped.len());
                }
                for row in grouped {
                    ui.add_space(Spacing::XS);
                    let change = ui
                        .push_id(("agent-row", &row.id), |ui| agents_access_row(ui, row))
                        .inner;
                    if let Some(access) = change {
                        self.access_changes.push((row.id.clone(), access));
                    }
                }
                ui.add_space(Spacing::XS);
            }
        });
    }

    /// Buttons that open something else, at the end of the section they belong to.
    fn section_actions(&mut self, ui: &mut egui::Ui) {
        match self.section {
            "General" => {
                ui.add_space(Spacing::S);
                ui.horizontal(|ui| {
                    ui.add_space(9.0 + 2.0 * Spacing::M);
                    if ui
                        .button("Check for updates now")
                        .on_hover_text("Asks GitHub for the latest stable release")
                        .clicked()
                    {
                        self.check_updates = true;
                    }
                    theme::hint(
                        ui,
                        &format!("You are running sqlr {}", env!("CARGO_PKG_VERSION")),
                    );
                });
            }
            "Drivers" => {
                ui.add_space(Spacing::M);
                ui.horizontal(|ui| {
                    if ui
                        .button("Drivers…")
                        .on_hover_text("Install drivers from local files, and remove them")
                        .clicked()
                    {
                        self.open_drivers = true;
                    }
                    if ui
                        .button("Databases…")
                        .on_hover_text("The databases sqlr connects to, and those planned")
                        .clicked()
                    {
                        self.open_databases = true;
                    }
                });
            }
            _ => {}
        }
    }

    fn persist_all(
        &mut self,
        live: &LayeredSettings,
        dir: &Path,
        keymap: &sqlr_store::StoredKeymap,
        close: bool,
        ctx: &egui::Context,
    ) {
        if !keymap
            .keymap
            .conflicts(cfg!(target_os = "macos"))
            .is_empty()
        {
            self.error = Some("Resolve conflicting shortcuts before saving".into());
            return;
        }
        let live = live.clone();
        let keymap = keymap.clone();
        let dir = dir.to_path_buf();
        let (sender, receiver) = std::sync::mpsc::channel();
        self.saving = Some(PendingSave {
            receiver,
            live: live.clone(),
            keymap: keymap.clone(),
            close,
            frame: ctx.cumulative_frame_nr(),
        });
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = live.save(&dir).and_then(|()| keymap.save(&dir));
            let _ = sender.send(result);
            repaint.request_repaint();
        });
    }
}

/// The color that tells a connection's exposure apart at a glance.
fn agent_access_color(p: &theme::Palette, access: AgentAccess) -> Color32 {
    match access {
        AgentAccess::None => p.text_disabled,
        AgentAccess::Read => p.accent,
        AgentAccess::Write => p.warning,
    }
}

/// A folder heading in Settings → Agents' connection list.
fn agents_folder_header(ui: &mut egui::Ui, folder: Option<&str>, connections: usize) {
    let p = theme::palette(ui.ctx());
    ui.add_space(Spacing::M);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        ui.label(icons::text(ui, icons::FOLDER, 12.0).color(p.text_muted));
        ui.label(
            egui::RichText::new(folder.unwrap_or("Ungrouped"))
                .size(11.0)
                .strong()
                .color(p.text_muted),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                egui::RichText::new(connections.to_string())
                    .size(11.0)
                    .color(p.text_muted),
            );
        });
    });
    ui.add_space(Spacing::XS);
}

/// One connection with its exposure control at the right.
fn agents_access_row(ui: &mut egui::Ui, row: &AgentAccessRow) -> Option<AgentAccess> {
    /// Three 46 px buttons, their 2 px gaps, and the slack of the widest label.
    const PICKER_WIDTH: f32 = 3.0 * 48.0 + 2.0 * 2.0;
    let p = theme::palette(ui.ctx());
    let mut change = None;
    Frame::new()
        .fill(p.bg_subtle)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(Spacing::RADIUS_S))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                icons::paint_database(ui, &row.driver);
                ui.add_space(Spacing::M);
                let text_width = (ui.available_width() - PICKER_WIDTH - Spacing::M).max(80.0);
                ui.allocate_ui_with_layout(
                    vec2(text_width, 34.0),
                    Layout::top_down(Align::LEFT),
                    |ui| {
                        // Reserve the whole column: the picker is then flush with the
                        // card's right edge, not right after the name.
                        ui.set_width(text_width);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = Spacing::S;
                            ui.label(
                                egui::RichText::new(icons::DOT)
                                    .size(8.0)
                                    .color(agent_access_color(&p, row.access)),
                            );
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&row.name).size(12.5).color(p.text),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&row.name);
                        });
                        let meta = match &row.profile {
                            Some(profile) => format!("{} · {profile}", row.driver_name),
                            None => row.driver_name.clone(),
                        };
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(meta).size(10.5).color(p.text_muted),
                            )
                            .truncate(),
                        );
                    },
                );
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), 24.0),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        ui.set_width(ui.available_width());
                        change = agents_access_picker(ui, &row.name, row.access);
                    },
                );
            });
        });
    change
}

/// The three-level exposure control. Its buttons are named after the connection so screen
/// readers and UI tests can tell rows apart.
fn agents_access_picker(
    ui: &mut egui::Ui,
    connection_name: &str,
    current: AgentAccess,
) -> Option<AgentAccess> {
    let p = theme::palette(ui.ctx());
    let mut picked = None;
    ui.spacing_mut().item_spacing.x = 2.0;
    // Added right to left so they read None · Read · Write, flush with the card's
    // right edge.
    for (value, label, tip) in [
        (
            AgentAccess::Write,
            "Write",
            "Agents may write; the profile's confirmations still apply",
        ),
        (
            AgentAccess::Read,
            "Read",
            "Agents may browse and run reads only",
        ),
        (
            AgentAccess::None,
            "None",
            "Agents cannot see this connection",
        ),
    ] {
        let active = current == value;
        let (fill, text) = if active {
            let tint = agent_access_color(&p, value);
            (p.bg_panel.lerp_to_gamma(tint, 0.22), p.text)
        } else {
            (Color32::TRANSPARENT, p.text_muted)
        };
        let font = if active {
            theme::semibold(ui.ctx(), 11.5)
        } else {
            egui::FontId::proportional(11.5)
        };
        let button = egui::Button::new(egui::RichText::new(label).font(font).color(text))
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(Spacing::RADIUS_S))
            .min_size(vec2(46.0, 24.0));
        let response = ui.add(button);
        response.widget_info(|| {
            WidgetInfo::selected(
                WidgetType::Button,
                true,
                active,
                format!("{connection_name}: {label}"),
            )
        });
        if response.clicked() {
            picked = Some(value);
        }
        response.on_hover_text(tip);
    }
    picked
}
