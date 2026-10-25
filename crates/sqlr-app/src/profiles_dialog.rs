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
    confirm_reset: bool,
}

impl ProfilesDialog {
    fn editing(profile: &ConnectionProfile) -> Self {
        Self {
            draft: profile.clone(),
            new: false,
            error: None,
            confirm_reset: false,
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
        let mut action = None;
        let profiles = &self.profiles;
        let response = theme::Dialog::new(
            "connection_profiles",
            theme::icons::CONNECTION,
            "Connection profiles",
        )
        .subtitle("Colors and transaction behavior of connections")
        .width(740.0)
        .body_height(460.0)
        .min_body_height(240.0)
        .body_margin(Margin::ZERO)
        .show(ctx, |d| {
            if dialog.confirm_reset {
                d.top(|ui| {
                    ui.label(format!("Reset only {} to its defaults?", dialog.draft.name));
                    ui.horizontal(|ui| {
                        if ui.button("Cancel reset").clicked() {
                            dialog.confirm_reset = false;
                        }
                        if ui.button("Confirm reset").clicked() {
                            action = Some(ProfileAction::Delete);
                        }
                    });
                });
            }
            d.body(|ui| {
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
                            // Leaves room for "New profile" under the list.
                            .max_height((ui.available_height() - 40.0).max(60.0))
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
                                    theme::hint(ui, crate::transaction::mode_help(d.commit_mode));
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
            let blocked = dialog
                .draft
                .name
                .trim()
                .is_empty()
                .then_some("Enter a name");
            d.footer(|f| {
                let builtin = ConnectionProfile::is_builtin(&dialog.draft.id);
                let label = if builtin {
                    "Reset to defaults"
                } else {
                    "Delete"
                };
                if f.secondary(label, !dialog.new) {
                    if builtin {
                        dialog.confirm_reset = true;
                    } else {
                        action = Some(ProfileAction::Delete);
                    }
                }
                if f.primary("Save", blocked) {
                    action = Some(ProfileAction::Save);
                }
                f.cancel();
            });
        });
        if response.closed && action.is_none() {
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
                    confirm_reset: false,
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
    fn resetting_a_profile_requires_a_second_action() {
        use egui_kittest::kittest::Queryable;
        let (mut app, _dir) = crate::palette::tests::bare_app();
        app.open_profiles();
        let mut h = egui_kittest::Harness::builder()
            .with_size((1000.0, 850.0))
            .build_ui_state(|ui, app: &mut SqlrApp| app.profiles_ui(ui.ctx()), app);
        h.run_steps(3);
        h.get_by_label("Reset to defaults").click();
        h.run_steps(2);
        h.get_by_label("Confirm reset");
        h.get_by_label("Cancel reset").click();
        h.run_steps(2);
        assert!(h.query_by_label("Confirm reset").is_none());
    }

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
            check_updates: false,
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
