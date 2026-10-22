//! The role list and the role form. The form follows the object editors' pattern: it holds
//! the role as loaded (`before`, `None` for a new role) and an edited copy (`after`); the
//! pending change is their difference. Only the attributes the driver lists are shown. A
//! typed password goes into `after.password` (masked in `Debug` and in previews).

use egui::{RichText, TextEdit};
use sqlr_core::{AdminSupport, Password, RoleDef, RoleField};

use super::{ROW, caption};
use crate::theme::{self, Spacing};

pub struct RoleEditorState {
    /// The role as loaded; `None` while it is being created.
    pub before: Option<RoleDef>,
    pub after: RoleDef,
    /// The password field's text.
    password: String,
    /// Focus the name field on the next frame.
    focus_name: bool,
}

impl RoleEditorState {
    /// A form on a loaded role.
    pub fn new(role: RoleDef) -> Self {
        Self {
            before: Some(role.clone()),
            after: role,
            password: String::new(),
            focus_name: false,
        }
    }

    /// A form for a new role that can log in; with `hosts` (MySQL), for any host.
    pub fn new_role(hosts: bool) -> Self {
        Self {
            before: None,
            after: RoleDef {
                login: true,
                inherit: true,
                host: hosts.then(|| "%".to_string()),
                ..Default::default()
            },
            password: String::new(),
            focus_name: true,
        }
    }

    pub fn is_new(&self) -> bool {
        self.before.is_none()
    }

    /// The form differs from the role as loaded (a new role is always pending).
    pub fn pending(&self) -> bool {
        self.before.as_ref() != Some(&self.after)
    }

    /// Drops every edit, the typed password included.
    pub fn revert(&mut self) {
        if let Some(before) = &self.before {
            self.after = before.clone();
        }
        self.password.clear();
        self.after.password = None;
    }

    /// What blocks applying the form.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.after.name.trim().is_empty() {
            out.push("The name is empty".to_string());
        }
        out
    }
}

/// Edits an optional text: empty means `None`.
fn optional(ui: &mut egui::Ui, value: &mut Option<String>, hint: &str) {
    let mut text = value.clone().unwrap_or_default();
    if ui
        .add(
            TextEdit::singleline(&mut text)
                .desired_width(220.0)
                .hint_text(hint),
        )
        .changed()
    {
        *value = (!text.trim().is_empty()).then(|| text.trim().to_string());
    }
}

/// The form; `roles` are the grantee names the role can be a member of.
pub fn role_form(
    ui: &mut egui::Ui,
    state: &mut RoleEditorState,
    support: &AdminSupport,
    roles: &[String],
) {
    let has = |f: RoleField| support.role_fields.contains(&f);
    let muted = theme::palette(ui.ctx()).text_muted;
    let label = |ui: &mut egui::Ui, text: &str| {
        ui.label(RichText::new(text).color(muted));
    };
    let new = state.is_new();
    // A MariaDB role (no host) has no password and no connection limit.
    let account = !(support.hosts && state.after.host.is_none());
    egui::Grid::new("role_form")
        .num_columns(2)
        .spacing([Spacing::L, Spacing::S])
        .show(ui, |ui| {
            label(ui, "Name");
            let name = ui.add(
                TextEdit::singleline(&mut state.after.name)
                    .desired_width(220.0)
                    .hint_text("name"),
            );
            if std::mem::take(&mut state.focus_name) {
                name.request_focus();
            }
            ui.end_row();
            if support.hosts {
                label(ui, "Host");
                optional(ui, &mut state.after.host, "role (no host)");
                ui.end_row();
            }
            if has(RoleField::Password) && account {
                label(ui, "Password");
                ui.horizontal(|ui| {
                    let hint = if new { "none" } else { "unchanged" };
                    let field = ui.add_enabled(
                        !state.after.clear_password,
                        TextEdit::singleline(&mut state.password)
                            .password(true)
                            .desired_width(220.0)
                            .hint_text(hint),
                    );
                    if field.changed() {
                        state.after.password = (!state.password.is_empty())
                            .then(|| Password::new(state.password.clone()));
                    }
                    if !new
                        && ui
                            .checkbox(&mut state.after.clear_password, "Remove the password")
                            .changed()
                        && state.after.clear_password
                    {
                        state.password.clear();
                        state.after.password = None;
                    }
                });
                ui.end_row();
            }
            let flags = [
                (RoleField::Login, "Login", &mut state.after.login),
                (
                    RoleField::Superuser,
                    "Superuser",
                    &mut state.after.superuser,
                ),
                (
                    RoleField::CreateDb,
                    "Create databases",
                    &mut state.after.create_db,
                ),
                (
                    RoleField::CreateRole,
                    "Create roles",
                    &mut state.after.create_role,
                ),
                (RoleField::Inherit, "Inherit", &mut state.after.inherit),
                (
                    RoleField::Replication,
                    "Replication",
                    &mut state.after.replication,
                ),
            ];
            let shown: Vec<_> = flags.into_iter().filter(|(f, _, _)| has(*f)).collect();
            if !shown.is_empty() {
                label(ui, "Attributes");
                ui.horizontal_wrapped(|ui| {
                    for (_, text, value) in shown {
                        ui.checkbox(value, text);
                    }
                });
                ui.end_row();
            }
            if has(RoleField::ConnectionLimit) && account {
                label(ui, "Connection limit");
                let mut text = state
                    .after
                    .connection_limit
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                let valid = text.trim().is_empty() || text.trim().parse::<i32>().is_ok();
                let mut edit = TextEdit::singleline(&mut text)
                    .desired_width(120.0)
                    .hint_text("no limit");
                if !valid {
                    edit = edit.text_color(theme::palette(ui.ctx()).error);
                }
                if ui.add(edit).changed() {
                    let t = text.trim();
                    if t.is_empty() {
                        state.after.connection_limit = None;
                    } else if let Ok(n) = t.parse() {
                        state.after.connection_limit = Some(n);
                    }
                }
                ui.end_row();
            }
            if has(RoleField::ValidUntil) {
                label(ui, "Valid until");
                optional(ui, &mut state.after.valid_until, "never");
                ui.end_row();
            }
        });
    if has(RoleField::MemberOf) {
        ui.add_space(Spacing::S);
        label(ui, "Member of");
        let me = state.after.grantee();
        let others: Vec<&String> = roles.iter().filter(|r| **r != me).collect();
        if others.is_empty() {
            caption(ui, "No other role");
        }
        egui::ScrollArea::vertical()
            .id_salt("role_member_of")
            .max_height(140.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for role in others {
                        let mut member = state.after.member_of.contains(role);
                        if ui.checkbox(&mut member, role.as_str()).changed() {
                            if member {
                                state.after.member_of.push(role.clone());
                                state.after.member_of.sort();
                            } else {
                                state.after.member_of.retain(|r| r != role);
                            }
                        }
                    }
                });
            });
    }
}

/// The roles, filtered by `filter`; returns whether the selection (by grantee name)
/// changed. The role `current_user` logged in as reads "(you)".
pub fn role_list(
    ui: &mut egui::Ui,
    roles: &[RoleDef],
    selected: &mut Option<String>,
    filter: &mut String,
    current_user: Option<&str>,
) -> bool {
    let mut changed = false;
    let muted = theme::palette(ui.ctx()).text_muted;
    ui.add(
        TextEdit::singleline(filter)
            .hint_text("Filter roles")
            .desired_width(f32::INFINITY),
    );
    let needle = filter.to_lowercase();
    // Predefined roles (PostgreSQL's pg_*) can be joined but are not managed here.
    let shown: Vec<&RoleDef> = roles
        .iter()
        .filter(|r| !r.is_predefined())
        .filter(|r| needle.is_empty() || r.grantee().to_lowercase().contains(&needle))
        .collect();
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical()
        .id_salt("role_list")
        .auto_shrink([false, false])
        .show_rows(ui, ROW, shown.len(), |ui, range| {
            for r in &shown[range] {
                let grantee = r.grantee();
                let mut text = grantee.clone();
                if current_user == Some(r.name.as_str()) {
                    text.push_str(" (you)");
                }
                let mut flags = Vec::new();
                if r.superuser {
                    flags.push("superuser");
                }
                if r.login {
                    flags.push("can log in");
                }
                ui.horizontal(|ui| {
                    ui.set_min_height(ROW);
                    let is = selected.as_deref() == Some(grantee.as_str());
                    if ui.selectable_label(is, text).clicked() && !is {
                        *selected = Some(grantee.clone());
                        changed = true;
                    }
                    if !flags.is_empty() {
                        ui.label(RichText::new(flags.join(" · ")).small().color(muted));
                    }
                });
            }
        });
    changed
}
