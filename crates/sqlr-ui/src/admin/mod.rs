//! Administration widgets (spec §8.4): the role list and form, the
//! privilege grid, the sessions table, the locks list and blocking tree, server properties
//! and statistics. Like every widget they take data in and return actions; they never call
//! the engine. Long lists are virtualized (only visible rows are built).

mod locks;
mod privileges;
mod properties;
mod roles;
mod sessions;
mod stats;

pub use locks::{blocking_tree, locks_view};
pub use privileges::{PrivilegeCell, PrivilegeGridState, PrivilegeRow, privilege_grid};
pub use properties::{PropertiesState, properties_view};
pub use roles::{RoleEditorState, role_form, role_list};
pub use sessions::{SessionsAction, SessionsState, sessions_table};
pub use stats::{
    StatsColumn, StatsState, database_sizes_view, human_bytes, table_stats_detail, table_stats_view,
};

use egui::{Color32, Label, RichText, WidgetText, vec2};
use sqlr_core::AdminSupport;

use crate::theme::{self, Spacing};

/// An administration panel of a connection, opened as a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AdminPanel {
    /// Users and roles, with their privileges.
    Users,
    /// Sessions and locks.
    Sessions,
    /// Server properties, database sizes and table statistics.
    Server,
}

impl AdminPanel {
    pub fn label(self) -> &'static str {
        match self {
            AdminPanel::Users => "Users and roles",
            AdminPanel::Sessions => "Sessions and locks",
            AdminPanel::Server => "Server properties",
        }
    }

    /// The panels a driver's support allows, in menu order.
    pub fn for_support(support: &AdminSupport) -> Vec<AdminPanel> {
        let mut out = Vec::new();
        if support.roles || support.grants {
            out.push(AdminPanel::Users);
        }
        if support.sessions || support.locks {
            out.push(AdminPanel::Sessions);
        }
        if support.properties || support.statistics {
            out.push(AdminPanel::Server);
        }
        out
    }
}

/// Row height of the administration tables.
pub(crate) const ROW: f32 = 24.0;

/// One table row, `ROW` high, laid out left to right; `fill` paints its background.
pub(crate) fn row(ui: &mut egui::Ui, fill: Option<Color32>, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), ROW),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_height(ROW);
            if let Some(fill) = fill {
                ui.painter().rect_filled(ui.max_rect(), 2.0, fill);
            }
            ui.spacing_mut().item_spacing.x = Spacing::S;
            add(ui);
        },
    );
}

/// A cell `width` wide holding `text`, cut with "…" when longer.
pub(crate) fn cell(ui: &mut egui::Ui, width: f32, text: impl Into<WidgetText>) -> egui::Response {
    ui.allocate_ui_with_layout(
        vec2(width, ROW),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(width);
            ui.add(Label::new(text).truncate())
        },
    )
    .inner
}

/// A column heading, `width` wide.
pub(crate) fn heading(ui: &mut egui::Ui, width: f32, text: &str) -> egui::Response {
    let muted = theme::palette(ui.ctx()).text_muted;
    cell(ui, width, RichText::new(text).strong().color(muted))
}

/// A muted caption.
pub(crate) fn caption(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(RichText::new(text).color(theme::palette(ui.ctx()).text_muted))
}

/// "No pending changes", "1 pending change", "N pending changes".
pub(crate) fn pending_text(n: usize) -> String {
    match n {
        0 => "No pending changes".into(),
        1 => "1 pending change".into(),
        n => format!("{n} pending changes"),
    }
}
