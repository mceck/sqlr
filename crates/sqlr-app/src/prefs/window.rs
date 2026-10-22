//! The preferences window: sections on the left, a search field across every setting, and
//! the level (global, profile or connection) the values are edited at. Changes apply live;
//! Cancel puts everything back.

use std::path::Path;

use egui::{Align, ComboBox, Frame, Id, Layout, Margin, RichText, Stroke, vec2};
use sqlr_core::{Level, SECTIONS, SettingDef, search_settings, setting_defs};
use sqlr_store::LayeredSettings;
use sqlr_ui::theme::{self, Spacing, icons};

use super::resolve::Scopes;
use super::sections::Editing;
use crate::dialogs::nav_item;

/// Edits the app's live settings; the app derives everything else from them each frame.
struct PendingSave {
    receiver: std::sync::mpsc::Receiver<Result<(), String>>,
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
    /// The search field takes the keyboard when the window opens.
    focus_search: bool,
    /// "Drivers…" was clicked; the app opens the driver manager.
    pub open_drivers: bool,
    pub open_databases: bool,
    /// "Open settings folder" was clicked; the app opens it with the OS.
    pub open_settings_folder: bool,
}

fn section_icon(section: &str) -> &'static str {
    match section {
        "Appearance" => icons::APPEARANCE,
        "Editor" => icons::QUERY,
        "Result grid" => icons::TABLE,
        "Data formats" => icons::COLUMN,
        "Connections" => icons::CONNECTION,
        "Drivers" => icons::DATABASE,
        _ => icons::SETTINGS,
    }
}

impl PrefsWindow {
    pub fn new(live: &LayeredSettings, scope: Level) -> Self {
        Self {
            saving: None,
            original: live.clone(),
            keymap: None,
            keymap_editor: Default::default(),
            query: String::new(),
            // Overrides start where the per-connection settings are.
            section: if scope == Level::Global {
                "Appearance"
            } else {
                SECTIONS[0]
            },
            scope,
            error: None,
            focus_search: true,
            open_drivers: false,
            open_databases: false,
            open_settings_folder: false,
        }
    }

    /// Returns `false` once the window should close. Apply and Save write the files; Save and
    /// Cancel close, and Cancel restores what the window opened with.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        live: &mut LayeredSettings,
        scopes: &Scopes,
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
                    Ok(result) => Some(result),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Some(Err("Preferences writer stopped".into()))
                    }
                    Err(_) => None,
                };
                if let Some(result) = result {
                    let pending = self.saving.take().unwrap();
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
        let mut open = true;
        let mut cancel = false;
        egui::Window::new("Preferences")
            .open(&mut open)
            .title_bar(false)
            .collapsible(false)
            .resizable(true)
            .default_size(vec2(820.0, 560.0))
            .default_pos(ctx.content_rect().center() - vec2(410.0, 280.0))
            .min_width(600.0)
            .min_height(400.0)
            .frame(Frame::window(&ctx.global_style()).inner_margin(Margin::ZERO))
            .show(ctx, |ui| {
                if self.saving.is_some() {
                    ui.disable();
                }
                ui.set_min_width(800.0);
                ui.set_min_height(520.0);
                if theme::dialog_header(
                    ui,
                    icons::SETTINGS,
                    "Preferences",
                    Some("Application, editor and per-connection preferences"),
                ) {
                    cancel = true;
                }

                egui::Panel::bottom(Id::new("prefs_footer"))
                    .frame(
                        Frame::new()
                            .fill(p.bg_panel)
                            .inner_margin(Margin::symmetric(16, 12)),
                    )
                    .show(ui, |ui| {
                        ui.painter().hline(
                            ui.max_rect().x_range(),
                            ui.max_rect().top() - 0.5,
                            Stroke::new(1.0, p.border),
                        );
                        ui.horizontal(|ui| {
                            if let Some(error) = &self.error {
                                ui.colored_label(p.error, error);
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = Spacing::M;
                                let save =
                                    ui.add(theme::primary_button(ui.ctx(), "Save")).clicked();
                                let apply = ui
                                    .button("Apply")
                                    .on_hover_text("Save the settings without closing")
                                    .clicked();
                                if (save || apply) && self.saving.is_none() {
                                    self.persist_all(live, config_dir, keymap, save, ui.ctx());
                                }
                                if ui.button("Cancel").clicked() {
                                    cancel = true;
                                }
                            });
                        });
                        if self.saving.is_some() {
                            ui.label("Saving preferences…");
                        }
                    });

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
                            let active = self.query.trim().is_empty() && self.section == *section;
                            if nav_item(ui, section_icon(section), section, active).clicked() {
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
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    if self.section == "Keymap" && self.query.trim().is_empty() {
                                        self.keymap_editor.show(
                                            ui,
                                            &mut keymap.keymap,
                                            &mut keymap.preset,
                                        );
                                    } else {
                                        self.page(ui, live, scopes);
                                    }
                                });
                        });
                });
            });
        if (cancel || !open) && self.saving.is_none() {
            *live = self.original.clone();
            if let Some(original) = &self.keymap {
                *keymap = original.clone();
            }
            return false;
        }
        true
    }

    fn page(&mut self, ui: &mut egui::Ui, live: &mut LayeredSettings, scopes: &Scopes) {
        self.scope_picker(ui, scopes);
        ui.add_space(Spacing::L);
        let searching = !self.query.trim().is_empty();
        let mut editing = Editing {
            live,
            scope: &self.scope,
            scopes,
            error: &mut self.error,
        };
        if searching {
            let found = search_settings(&self.query);
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
        editing.rows(ui, &defs);
        if self.scope == Level::Global {
            self.section_actions(ui);
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

    /// Buttons that open something else, at the end of the section they belong to.
    fn section_actions(&mut self, ui: &mut egui::Ui) {
        match self.section {
            "Appearance" => {
                ui.add_space(Spacing::M);
                if ui
                    .button("Open settings folder")
                    .on_hover_text(
                        "Shows settings.toml, overrides.toml, connections.toml and profiles.toml",
                    )
                    .clicked()
                {
                    self.open_settings_folder = true;
                }
            }
            "Drivers" => {
                ui.add_space(Spacing::M);
                ui.horizontal(|ui| {
                    if ui
                        .button("Drivers…")
                        .on_hover_text("Install, update and remove drivers")
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
                ui.add_space(Spacing::S);
                ui.label(
                    RichText::new("The registry URL applies after a restart.")
                        .small()
                        .weak(),
                );
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
            let result = live
                .save(&dir)
                .and_then(|()| keymap.save(&dir))
                .map_err(|e| e.to_string());
            let _ = sender.send(result);
            repaint.request_repaint();
        });
    }
}
