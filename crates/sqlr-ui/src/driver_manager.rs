//! The Driver Manager (spec §6.4): installed, available and built-in drivers in a
//! searchable list, with the selected driver's details, versions and actions on the right.
//! Pure view: the app builds the rows from the engine and applies the returned action.

use egui::{RichText, vec2};

use crate::theme::{self, Spacing};

/// What a running, finished or failed install looks like.
#[derive(Clone, Debug, PartialEq)]
pub enum DriverJobView {
    Running { done: u64, total: Option<u64> },
    Done,
    Failed(String),
}

/// One driver of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct DriverRow {
    pub id: String,
    pub name: String,
    pub family: String,
    /// Ships with sqlr: cannot be removed or replaced.
    pub builtin: bool,
    /// Installed versions, oldest first.
    pub installed: Vec<String>,
    pub active: Option<String>,
    /// The active version was installed from a file.
    pub manual: bool,
    /// The registry's version, when it lists the driver.
    pub available: Option<String>,
    pub update_available: bool,
    pub description: String,
    pub homepage: String,
    pub license: String,
    pub job: Option<DriverJobView>,
}

/// The registry index as the manager shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum IndexStatus {
    /// Not contacted: nothing is fetched until the user presses Refresh or Install.
    Idle,
    Loading,
    Ready,
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct DriverManagerState {
    pub search: String,
    pub selected: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DriverManagerAction {
    Refresh,
    Install(String),
    Update(String),
    Activate {
        id: String,
        version: String,
    },
    Remove {
        id: String,
        version: Option<String>,
    },
    InstallFromFile,
    ClearJob(String),
    /// Stops the driver's process (a hung or confused one); its connections fail and the
    /// next connect starts a fresh process.
    Restart(String),
}

fn matches(row: &DriverRow, search: &str) -> bool {
    let q = search.trim().to_lowercase();
    q.is_empty()
        || row.name.to_lowercase().contains(&q)
        || row.id.to_lowercase().contains(&q)
        || row.description.to_lowercase().contains(&q)
}

fn group(row: &DriverRow) -> &'static str {
    if row.builtin {
        "Built in"
    } else if row.active.is_some() {
        "Installed"
    } else {
        "Available"
    }
}

/// Draws the manager; returns what the user asked for this frame.
pub fn driver_manager(
    ui: &mut egui::Ui,
    state: &mut DriverManagerState,
    rows: &[DriverRow],
    index: &IndexStatus,
) -> Option<DriverManagerAction> {
    let palette = theme::palette(ui.ctx());
    let mut action = None;
    ui.horizontal(|ui| {
        crate::text_edit(
            ui,
            egui::TextEdit::singleline(&mut state.search)
                .hint_text("Search drivers")
                .desired_width(260.0),
        )
        .widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Search drivers")
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Install from file…").clicked() {
                action = Some(DriverManagerAction::InstallFromFile);
            }
            if ui.button("Refresh").clicked() {
                action = Some(DriverManagerAction::Refresh);
            }
        });
    });
    let status = match index {
        IndexStatus::Idle => Some((
            "The registry has not been contacted yet".to_string(),
            palette.text_muted,
        )),
        IndexStatus::Loading => Some(("Contacting the registry…".to_string(), palette.text_muted)),
        IndexStatus::Failed(e) => {
            Some((format!("Could not load the registry: {e}"), palette.error))
        }
        IndexStatus::Ready => None,
    };
    if let Some((text, color)) = status {
        ui.add_space(Spacing::XS);
        ui.add(egui::Label::new(RichText::new(&text).color(color)).truncate())
            .on_hover_text(text);
    }
    ui.add_space(Spacing::S);
    ui.separator();

    let filtered: Vec<&DriverRow> = rows.iter().filter(|r| matches(r, &state.search)).collect();
    if state
        .selected
        .as_ref()
        .is_none_or(|s| !filtered.iter().any(|r| &r.id == s))
    {
        state.selected = filtered.first().map(|r| r.id.clone());
    }
    // In a dialog the available height is unbounded: size the panes from the window.
    let height = (ui.ctx().content_rect().height() * 0.55).clamp(220.0, 480.0);
    let list_width = 230.0;
    let details_width = (ui.available_width() - list_width - Spacing::XL).max(240.0);
    ui.horizontal_top(|ui| {
        // Each pane is its own top-down column: a pane inherits the row's horizontal layout.
        let column = egui::Layout::top_down(egui::Align::Min);
        ui.allocate_ui_with_layout(vec2(list_width, height), column, |ui| {
            ui.set_width(list_width);
            egui::ScrollArea::vertical()
                .id_salt("driver_list")
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for heading in ["Installed", "Available", "Built in"] {
                        let group_rows: Vec<_> =
                            filtered.iter().filter(|r| group(r) == heading).collect();
                        if group_rows.is_empty() {
                            continue;
                        }
                        ui.add_space(Spacing::S);
                        ui.label(
                            RichText::new(heading.to_uppercase())
                                .font(theme::semibold(ui.ctx(), 10.5))
                                .color(palette.text_muted),
                        );
                        for row in group_rows {
                            let selected = state.selected.as_deref() == Some(&row.id);
                            ui.horizontal(|ui| {
                                if ui.selectable_label(selected, &row.name).clicked() {
                                    state.selected = Some(row.id.clone());
                                }
                                if row.update_available {
                                    ui.label(RichText::new("update").color(palette.accent));
                                }
                            });
                        }
                    }
                });
        });
        // `ui.separator()` would run the whole available height: unbounded in a dialog.
        let (line, _) = ui.allocate_exact_size(vec2(Spacing::M, height), egui::Sense::hover());
        ui.painter().vline(
            line.center().x,
            line.y_range(),
            egui::Stroke::new(1.0, palette.border),
        );
        ui.allocate_ui_with_layout(vec2(details_width, height), column, |ui| {
            ui.set_width(details_width);
            let selected = state
                .selected
                .as_ref()
                .and_then(|s| filtered.iter().find(|r| &r.id == s));
            egui::ScrollArea::vertical()
                .id_salt("driver_details")
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| match selected {
                    Some(row) => {
                        if let Some(a) = details(ui, row) {
                            action = Some(a);
                        }
                    }
                    None => theme::hint(ui, "No driver matches"),
                });
        });
    });
    action
}

fn details(ui: &mut egui::Ui, row: &DriverRow) -> Option<DriverManagerAction> {
    let palette = theme::palette(ui.ctx());
    let mut action = None;
    ui.label(RichText::new(&row.name).font(theme::semibold(ui.ctx(), 16.0)));
    ui.label(RichText::new(format!("{} · {}", row.family, row.id)).color(palette.text_muted));
    if !row.description.is_empty() {
        ui.add_space(Spacing::S);
        ui.label(&row.description);
    }
    ui.add_space(Spacing::S);
    if !row.license.is_empty() {
        ui.label(RichText::new(format!("License: {}", row.license)).color(palette.text_muted));
    }
    if !row.homepage.is_empty() {
        ui.hyperlink_to(&row.homepage, &row.homepage);
    }
    ui.add_space(Spacing::M);

    if row.builtin {
        ui.label(RichText::new("Built in").color(palette.success));
        theme::hint(ui, "This driver ships with sqlr.");
        return None;
    }

    match &row.job {
        Some(DriverJobView::Running { done, total }) => {
            let fraction = total.filter(|t| *t > 0).map(|t| *done as f32 / t as f32);
            let label = match fraction {
                Some(f) => format!("Installing… {}%", (f * 100.0).round() as u32),
                None => "Installing…".to_string(),
            };
            ui.add(egui::ProgressBar::new(fraction.unwrap_or(0.0)).text(label));
            return None;
        }
        Some(DriverJobView::Failed(message)) => {
            ui.label(RichText::new(format!("Install failed: {message}")).color(palette.error));
            if ui.button("Dismiss").clicked() {
                action = Some(DriverManagerAction::ClearJob(row.id.clone()));
            }
            ui.add_space(Spacing::S);
        }
        Some(DriverJobView::Done) | None => {}
    }

    if let Some(active) = &row.active {
        let source = if row.manual {
            " (installed from a file)"
        } else {
            ""
        };
        ui.label(RichText::new(format!("Active version {active}{source}")).color(palette.success));
    }
    match (&row.available, row.update_available, row.active.is_some()) {
        (Some(version), true, true) => {
            if ui.button(format!("Update to {version}")).clicked() {
                action = Some(DriverManagerAction::Update(row.id.clone()));
            }
        }
        (Some(version), _, false) => {
            theme::hint(ui, &format!("Version {version} is available."));
            if ui.add(theme::primary_button(ui.ctx(), "Install")).clicked() {
                action = Some(DriverManagerAction::Install(row.id.clone()));
            }
        }
        _ => {}
    }

    if !row.installed.is_empty() {
        ui.add_space(Spacing::M);
        theme::section_title(ui, "Versions");
        for version in &row.installed {
            let active = row.active.as_deref() == Some(version);
            ui.horizontal(|ui| {
                ui.label(if active {
                    format!("{version}  (active)")
                } else {
                    version.clone()
                });
                if !active && ui.small_button(format!("Activate {version}")).clicked() {
                    action = Some(DriverManagerAction::Activate {
                        id: row.id.clone(),
                        version: version.clone(),
                    });
                }
                if !active && ui.small_button(format!("Remove {version}")).clicked() {
                    action = Some(DriverManagerAction::Remove {
                        id: row.id.clone(),
                        version: Some(version.clone()),
                    });
                }
            });
        }
        ui.add_space(Spacing::M);
        if ui
            .button("Restart driver")
            .on_hover_text("Stops the driver's process; its connections fail and reconnect fresh")
            .clicked()
        {
            action = Some(DriverManagerAction::Restart(row.id.clone()));
        }
        if ui.button("Remove driver").clicked() {
            action = Some(DriverManagerAction::Remove {
                id: row.id.clone(),
                version: None,
            });
        }
    }
    action
}
