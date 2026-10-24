//! The connection profiles window: list, create, edit (name, color, commit mode, flags,
//! idle timeout), delete user profiles and reset built-ins. The engine writes
//! `profiles.toml` in the background; the list follows once it has reloaded.

use egui::{
    Align, Align2, Color32, CornerRadius, DragValue, FontId, Frame, Id, Layout, Margin, RichText,
    Sense, Stroke, TextEdit, WidgetInfo, WidgetType, vec2,
};
use sqlr_core::{CommitMode, ConnectionProfile};
use sqlr_ui::text_edit;
use sqlr_ui::theme::{self, Spacing};

use crate::app::SqlrApp;

const COLORS: [[u8; 3]; 6] = [
    [0xE0, 0x4F, 0x4F],
    [0xE0, 0xA0, 0x30],
    [0xD8, 0xC2, 0x4A],
    [0x5A, 0xB0, 0x5A],
    [0x4A, 0x8F, 0xD8],
    [0x9A, 0x6A, 0xD8],
];

pub struct ProfilesDialog {
    /// The profile being edited (a copy, saved on "Save").
    draft: ConnectionProfile,
    /// The draft is a new user profile not saved yet.
    new: bool,
    error: Option<String>,
}

impl ProfilesDialog {
    fn editing(profile: &ConnectionProfile) -> Self {
        Self {
            draft: profile.clone(),
            new: false,
            error: None,
        }
    }
}

/// What the window asked for this frame.
enum ProfileAction {
    Select(ConnectionProfile),
    New,
    Save,
    /// Delete a user profile, or reset a built-in.
    Delete,
    Close,
}

/// One profile in the left list: a color dot and the name.
fn profile_row(
    ui: &mut egui::Ui,
    name: &str,
    color: Option<[u8; 3]>,
    selected: bool,
) -> egui::Response {
    let p = theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let fill = if selected {
        p.selection
    } else if response.hovered() {
        p.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
    let dot = egui::pos2(rect.left() + 16.0, rect.center().y);
    match color {
        Some([r, g, b]) => {
            ui.painter()
                .circle_filled(dot, 4.5, Color32::from_rgb(r, g, b));
        }
        None => {
            ui.painter()
                .circle_stroke(dot, 4.5, Stroke::new(1.0, p.text_disabled));
        }
    }
    ui.painter().text(
        egui::pos2(rect.left() + 30.0, rect.center().y),
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(13.0),
        p.text,
    );
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, name));
    response.on_hover_text(name)
}

/// One row of the profile form: a fixed label column and its control, with a micro gap
/// around it so consecutive settings stay distinct.
fn setting_row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(Spacing::XS);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::L;
        ui.allocate_ui_with_layout(
            vec2(112.0, ui.spacing().interact_size.y),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_width(112.0);
                ui.label(label);
            },
        );
        add(ui);
    });
    ui.add_space(Spacing::S);
}

fn color_row(ui: &mut egui::Ui, color: &mut Option<[u8; 3]>) {
    ui.horizontal(|ui| {
        if ui.selectable_label(color.is_none(), "None").clicked() {
            *color = None;
        }
        for preset in COLORS {
            let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
            let [r, g, b] = preset;
            ui.painter()
                .circle_filled(rect.center(), 7.0, Color32::from_rgb(r, g, b));
            if *color == Some(preset) {
                ui.painter().circle_stroke(
                    rect.center(),
                    9.0,
                    egui::Stroke::new(1.5, ui.visuals().strong_text_color()),
                );
            }
            if response.clicked() {
                *color = Some(preset);
            }
        }
    });
}

impl SqlrApp {
    pub(crate) fn open_profiles(&mut self) {
        self.reload_profiles();
        let first = self
            .profiles
            .first()
            .cloned()
            .unwrap_or_else(ConnectionProfile::development);
        self.profiles_dialog = Some(ProfilesDialog::editing(&first));
    }

    pub(crate) fn profiles_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.profiles_dialog else {
            return;
        };
        let p = theme::palette(ctx);
        let mut open = true;
        let mut action = None;
        let profiles = &self.profiles;
        egui::Window::new("Connection profiles")
            .open(&mut open)
            .title_bar(false)
            .collapsible(false)
            .resizable(true)
            .default_size(vec2(
                720.0_f32.min(ctx.content_rect().width() - 32.0),
                540.0_f32.min(ctx.content_rect().height() - 32.0),
            ))
            .default_pos(
                ctx.content_rect().center()
                    - vec2(
                        360.0_f32.min((ctx.content_rect().width() - 32.0) / 2.0),
                        270.0_f32.min((ctx.content_rect().height() - 32.0) / 2.0),
                    ),
            )
            .min_width(560.0)
            .min_height(400.0)
            .frame(Frame::window(&ctx.global_style()).inner_margin(Margin::ZERO))
            .show(ctx, |ui| {
                ui.set_min_width(560.0_f32.min(ctx.content_rect().width() - 32.0));
                ui.set_min_height(400.0_f32.min(ctx.content_rect().height() - 32.0));
                if theme::dialog_header(
                    ui,
                    theme::icons::CONNECTION,
                    "Connection profiles",
                    Some("Colors and transaction behavior of connections"),
                ) {
                    action = Some(ProfileAction::Close);
                }
                egui::Panel::bottom(Id::new("profiles_footer"))
                    .frame(
                        Frame::new()
                            .fill(p.bg_panel)
                            .inner_margin(Margin::symmetric(16, 10)),
                    )
                    .show(ui, |ui| {
                        ui.painter().hline(
                            ui.max_rect().x_range(),
                            ui.max_rect().top() - 0.5,
                            Stroke::new(1.0, p.border),
                        );
                        let valid = !dialog.draft.name.trim().is_empty();
                        ui.horizontal(|ui| {
                            let builtin = ConnectionProfile::is_builtin(&dialog.draft.id);
                            let label = if builtin { "Reset" } else { "Delete" };
                            if ui
                                .add_enabled(!dialog.new, egui::Button::new(label))
                                .clicked()
                            {
                                action = Some(ProfileAction::Delete);
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let save = theme::primary_button_state(ui.ctx(), valid, "Save");
                                if ui.add_enabled(valid, save).clicked() {
                                    action = Some(ProfileAction::Save);
                                }
                                if ui.button("Close").clicked() {
                                    action = Some(ProfileAction::Close);
                                }
                            });
                        });
                    });

                egui::Panel::left(Id::new("profiles_list"))
                    .resizable(false)
                    .exact_size(200.0)
                    .frame(
                        Frame::new()
                            .fill(p.bg_subtle)
                            .inner_margin(Margin::same(10)),
                    )
                    .show(ui, |ui| {
                        ui.label(RichText::new("Profiles").font(theme::semibold(ui.ctx(), 12.0)));
                        ui.add_space(Spacing::S);
                        egui::ScrollArea::vertical()
                            .id_salt("profiles_scroll")
                            .auto_shrink(false)
                            .show(ui, |ui| {
                                for profile in profiles {
                                    let selected = !dialog.new && profile.id == dialog.draft.id;
                                    if profile_row(ui, &profile.name, profile.color, selected)
                                        .clicked()
                                    {
                                        action = Some(ProfileAction::Select(profile.clone()));
                                    }
                                }
                            });
                        ui.add_space(Spacing::S);
                        if ui.button("New profile").clicked() {
                            action = Some(ProfileAction::New);
                        }
                    });

                egui::CentralPanel::no_frame().show(ui, |ui| {
                    Frame::new()
                        .fill(p.bg_raised)
                        .inner_margin(Margin::symmetric(24, 20))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("profile_form")
                                .auto_shrink(false)
                                .show(ui, |ui| {
                                    let d = &mut dialog.draft;
                                    theme::section_title(ui, "Profile");
                                    theme::hint(
                                        ui,
                                        "Color and transaction behavior of its connections.",
                                    );
                                    ui.add_space(Spacing::L);
                                    setting_row(ui, "Name", |ui| {
                                        text_edit(
                                            ui,
                                            TextEdit::singleline(&mut d.name)
                                                .desired_width(f32::INFINITY),
                                        );
                                    });
                                    setting_row(ui, "Color", |ui| color_row(ui, &mut d.color));
                                    ui.add_space(Spacing::M);
                                    theme::hairline(ui);
                                    ui.add_space(Spacing::M);
                                    theme::section_title(ui, "Behavior");
                                    ui.add_space(Spacing::S);
                                    setting_row(ui, "Commit mode", |ui| {
                                        for (m, label) in [
                                            (CommitMode::Auto, "Auto"),
                                            (CommitMode::Manual, "Manual"),
                                            (CommitMode::Smart, "Smart"),
                                        ] {
                                            if theme::segment(ui, d.commit_mode == m, label)
                                                .clicked()
                                            {
                                                d.commit_mode = m;
                                            }
                                        }
                                    });
                                    setting_row(ui, "Safety", |ui| {
                                        ui.vertical(|ui| {
                                            ui.checkbox(
                                                &mut d.confirm_writes,
                                                "Confirm statements that modify data",
                                            );
                                            ui.checkbox(
                                                &mut d.confirm_destructive,
                                                "Confirm destructive statements",
                                            );
                                            ui.checkbox(&mut d.read_only, "Read-only sessions");
                                        });
                                    });
                                    let mut timeout = d.idle_tx_timeout_min.is_some();
                                    setting_row(ui, "Idle transactions", |ui| {
                                        ui.checkbox(&mut timeout, "Ask after");
                                        let mut minutes = d.idle_tx_timeout_min.unwrap_or(10);
                                        ui.add_enabled(
                                            timeout,
                                            DragValue::new(&mut minutes).range(1..=1440),
                                        );
                                        ui.label("min");
                                        d.idle_tx_timeout_min = timeout.then_some(minutes);
                                    });
                                    if let Some(error) = &dialog.error {
                                        ui.add_space(Spacing::S);
                                        ui.colored_label(p.error, error);
                                    }
                                });
                        });
                });
            });
        if !open {
            action = Some(ProfileAction::Close);
        }
        match action {
            None => {}
            Some(ProfileAction::Close) => self.profiles_dialog = None,
            Some(ProfileAction::Select(p)) => {
                self.profiles_dialog = Some(ProfilesDialog::editing(&p));
            }
            Some(ProfileAction::New) => {
                let draft = ConnectionProfile {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: "New profile".into(),
                    ..ConnectionProfile::development()
                };
                self.profiles_dialog = Some(ProfilesDialog {
                    draft,
                    new: true,
                    error: None,
                });
            }
            Some(ProfileAction::Save) => {
                let mut draft = dialog.draft.clone();
                draft.name = draft.name.trim().to_string();
                self.engine
                    .save_profile(self.config_dir.clone(), draft.clone());
                self.profiles_dialog = Some(ProfilesDialog::editing(&draft));
            }
            Some(ProfileAction::Delete) => {
                let id = dialog.draft.id.clone();
                if let Some(error) = self.profile_in_use(&id) {
                    if let Some(dialog) = &mut self.profiles_dialog {
                        dialog.error = Some(error);
                    }
                    return;
                }
                self.engine
                    .delete_profile(self.config_dir.clone(), id.clone());
                // A reset built-in shows its defaults; a deleted profile shows the first.
                let next = ConnectionProfile::builtins()
                    .into_iter()
                    .find(|p| p.id == id)
                    .unwrap_or_else(ConnectionProfile::development);
                self.profiles_dialog = Some(ProfilesDialog::editing(&next));
            }
        }
    }

    /// Why the user profile `id` cannot be deleted: the connections that use it. A built-in
    /// can always be reset.
    pub(crate) fn profile_in_use(&self, id: &str) -> Option<String> {
        if ConnectionProfile::is_builtin(id) {
            return None;
        }
        let users: Vec<&str> = self
            .connections
            .iter()
            .filter(|c| c.profile.as_deref() == Some(id))
            .map(|c| c.name.as_str())
            .collect();
        (!users.is_empty()).then(|| format!("In use by {}", users.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlr_core::{ConnectionConfig, Secrets};
    use sqlr_engine::{DriverRegistry, Engine};
    use sqlr_store::{ConnectionRepo, MemorySecretStore, Settings};

    use super::*;
    use crate::app::AppDeps;

    #[test]
    fn a_profile_in_use_cannot_be_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let repo =
            ConnectionRepo::open(dir.path(), Box::new(MemorySecretStore::default())).unwrap();
        for (id, name, profile) in [("1", "orders", "mine"), ("2", "billing", "mine")] {
            let cfg = ConnectionConfig {
                id: id.into(),
                name: name.into(),
                driver: "fake".into(),
                group: None,
                color: None,
                values: Default::default(),
                profile: Some(profile.into()),
                save_secrets: true,
                agent_access: Default::default(),
            };
            repo.save(&cfg, &Secrets::default()).unwrap();
        }
        let deps = AppDeps {
            engine: Engine::new(DriverRegistry::default(), Arc::new(|| {})),
            repo,
            settings: Settings::default(),
            config_dir: dir.path().to_path_buf(),
            data_dir: None,
            startup_notice: None,
            storage_warning: None,
            mcp_token_store: None,
            diagnostics: None,
            diagnostics_warning: None,
        };
        let app = SqlrApp::new(deps, &egui::Context::default());
        assert_eq!(
            app.profile_in_use("mine").as_deref(),
            Some("In use by orders, billing")
        );
        assert_eq!(app.profile_in_use("other"), None);
        // A built-in can always be reset.
        assert_eq!(app.profile_in_use("production"), None);
    }
}
