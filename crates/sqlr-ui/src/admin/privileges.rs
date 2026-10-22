//! The privilege grid: one row per (grantee, object) pair (a role's objects, or an object's
//! roles), one column per privilege the driver lists for the rows' kinds, and tri-state
//! cells (none, granted, grantable) that cycle on click. Pending changes are derived from
//! the grants as loaded and as edited, so clicking a cell back removes its change.

use std::collections::{BTreeSet, HashMap};

use egui::{Align2, ComboBox, Sense, TextEdit, WidgetInfo, WidgetType, vec2};
use sqlr_core::{AdminSupport, Grant, ObjectKind, ObjectRef};

use super::{ROW, caption, cell, heading, pending_text, row};
use crate::grid::change_tint;
use crate::theme::{self, Spacing, icons};
use crate::tree::kind_label;

/// A row of the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivilegeRow {
    pub grantee: String,
    pub object: ObjectRef,
    /// What the row shows: the object's qualified name, or the role's.
    pub label: String,
}

/// One cell's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivilegeCell {
    None,
    Granted,
    /// Granted with the right to grant it on.
    Grantable,
}

impl PrivilegeCell {
    fn next(self) -> Self {
        match self {
            PrivilegeCell::None => PrivilegeCell::Granted,
            PrivilegeCell::Granted => PrivilegeCell::Grantable,
            PrivilegeCell::Grantable => PrivilegeCell::None,
        }
    }

    fn of(grantable: Option<&bool>) -> Self {
        match grantable {
            None => PrivilegeCell::None,
            Some(false) => PrivilegeCell::Granted,
            Some(true) => PrivilegeCell::Grantable,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            PrivilegeCell::None => "not granted",
            PrivilegeCell::Granted => "granted",
            PrivilegeCell::Grantable => "granted with grant option",
        }
    }
}

/// `(grantee, object, privilege)`.
type Key = (String, ObjectRef, String);

/// The grants as loaded and as edited, and the grid's filters.
#[derive(Default)]
pub struct PrivilegeGridState {
    /// Grant option by key.
    before: HashMap<Key, bool>,
    after: HashMap<Key, bool>,
    /// Case-insensitive substring of the rows' labels.
    pub filter: String,
    /// Only rows of this kind.
    pub kind: Option<ObjectKind>,
    /// Scrolls this row (of the filtered rows) to the top on the next frame.
    pub scroll_to: Option<usize>,
    /// The caller shows the pending count itself.
    pub hide_pending: bool,
}

fn key(grantee: &str, object: &ObjectRef, privilege: &str) -> Key {
    (grantee.to_string(), object.clone(), privilege.to_string())
}

/// A sort key for grants, so changes come out in a stable order.
fn order(k: &Key) -> (String, Option<String>, String, Option<String>, String) {
    (
        k.0.clone(),
        k.1.schema.clone(),
        k.1.name.clone(),
        k.1.signature.clone(),
        k.2.clone(),
    )
}

impl PrivilegeGridState {
    /// The grants as loaded: nothing is pending afterwards.
    pub fn set_grants(&mut self, grants: &[Grant]) {
        self.before.clear();
        for g in grants {
            let grantable = self
                .before
                .entry(key(&g.grantee, &g.object, &g.privilege))
                .or_insert(false);
            *grantable |= g.grantable;
        }
        self.after = self.before.clone();
    }

    pub fn cell(&self, grantee: &str, object: &ObjectRef, privilege: &str) -> PrivilegeCell {
        PrivilegeCell::of(self.after.get(&key(grantee, object, privilege)))
    }

    pub fn set(&mut self, grantee: &str, object: &ObjectRef, privilege: &str, to: PrivilegeCell) {
        let k = key(grantee, object, privilege);
        match to {
            PrivilegeCell::None => self.after.remove(&k),
            PrivilegeCell::Granted => self.after.insert(k, false),
            PrivilegeCell::Grantable => self.after.insert(k, true),
        };
    }

    /// Whether a cell differs from how it was loaded.
    fn changed(&self, grantee: &str, object: &ObjectRef, privilege: &str) -> bool {
        let k = key(grantee, object, privilege);
        self.before.get(&k) != self.after.get(&k)
    }

    /// The grants to add and to revoke. A revoke with `grantable` revokes only the grant
    /// option (see [`sqlr_core::Admin::grant_ddl`]); a grantable privilege removed
    /// entirely also revokes the option when no other privilege on the object keeps it.
    pub fn changes(&self) -> (Vec<Grant>, Vec<Grant>) {
        let mut keys: Vec<&Key> = self.before.keys().chain(self.after.keys()).collect();
        keys.sort_by_key(|k| order(k));
        keys.dedup();
        let mut add = Vec::new();
        let mut revoke = Vec::new();
        for k in keys {
            let grant = |grantable| Grant {
                grantee: k.0.clone(),
                object: k.1.clone(),
                privilege: k.2.clone(),
                grantable,
            };
            match (self.before.get(k), self.after.get(k)) {
                (None, Some(g)) => add.push(grant(*g)),
                (Some(false), Some(true)) => add.push(grant(true)),
                (Some(true), Some(false)) => revoke.push(grant(true)),
                (Some(false), None) => revoke.push(grant(false)),
                (Some(true), None) => {
                    revoke.push(grant(false));
                    // The object's last grantable privilege: its grant option goes too
                    // (MySQL keeps the option per object, not per privilege).
                    let option_left = self
                        .after
                        .iter()
                        .any(|(o, grantable)| *grantable && o.0 == k.0 && o.1 == k.1);
                    if !option_left {
                        revoke.push(grant(true));
                    }
                }
                _ => {}
            }
        }
        (add, revoke)
    }

    /// How many cells changed.
    pub fn pending(&self) -> usize {
        self.before
            .keys()
            .chain(self.after.keys())
            .filter(|k| self.before.get(*k) != self.after.get(*k))
            .map(order)
            .collect::<BTreeSet<_>>()
            .len()
    }

    /// Drops every edit.
    pub fn revert(&mut self) {
        self.after = self.before.clone();
    }

    /// The grantees of the grants as loaded, sorted.
    pub fn grantees(&self) -> Vec<String> {
        self.before
            .keys()
            .map(|(g, _, _)| g.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn shown<'a>(&self, rows: &'a [PrivilegeRow]) -> Vec<&'a PrivilegeRow> {
        let filter = self.filter.to_lowercase();
        rows.iter()
            .filter(|r| self.kind.is_none_or(|k| r.object.kind == k))
            .filter(|r| filter.is_empty() || r.label.to_lowercase().contains(&filter))
            .collect()
    }
}

/// Width of the row label column and of each privilege column.
const W_LABEL: f32 = 240.0;
const W_CELL: f32 = 96.0;

/// Draws the grid; returns whether a cell changed.
pub fn privilege_grid(
    ui: &mut egui::Ui,
    state: &mut PrivilegeGridState,
    rows: &[PrivilegeRow],
    support: &AdminSupport,
) -> bool {
    let mut changed = false;
    // Toolbar: filters and counts.
    let mut kinds: Vec<ObjectKind> = Vec::new();
    for r in rows {
        if !kinds.contains(&r.object.kind) {
            kinds.push(r.object.kind);
        }
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        icons::paint(ui, icons::SEARCH, theme::palette(ui.ctx()).text_muted);
        ui.add(
            TextEdit::singleline(&mut state.filter)
                .hint_text("Filter")
                .desired_width(180.0),
        );
        if kinds.len() > 1 {
            let text = state.kind.map_or("All kinds", kind_label);
            ComboBox::from_id_salt("privilege_kind")
                .selected_text(text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut state.kind, None, "All kinds");
                    for k in &kinds {
                        ui.selectable_value(&mut state.kind, Some(*k), kind_label(*k));
                    }
                });
        }
    });
    let shown = state.shown(rows);
    // The privileges of the shown rows' kinds, in the driver's order.
    let mut columns: Vec<&str> = Vec::new();
    for (kind, privileges) in &support.privileges {
        if shown.iter().any(|r| r.object.kind == *kind) {
            for p in privileges {
                if !columns.contains(&p.as_str()) {
                    columns.push(p);
                }
            }
        }
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::M;
        let count = if shown.len() == rows.len() {
            format!("{} rows", rows.len())
        } else {
            format!("{} of {} rows", shown.len(), rows.len())
        };
        caption(ui, &count);
        if !state.hide_pending {
            let n = state.pending();
            let p = theme::palette(ui.ctx());
            let color = if n > 0 { p.warning } else { p.text_muted };
            ui.label(egui::RichText::new(pending_text(n)).color(color));
        }
        caption(
            ui,
            "Click a cell: not granted → granted → with grant option",
        );
    });
    ui.add_space(Spacing::S);

    let mut area = egui::ScrollArea::both()
        .id_salt("privilege_grid")
        .auto_shrink([false, false]);
    if let Some(at) = state.scroll_to.take() {
        area = area.vertical_scroll_offset(at as f32 * ROW);
    }
    // Header.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        heading(ui, W_LABEL, "Object");
        for p in &columns {
            heading(ui, W_CELL, p);
        }
    });
    ui.spacing_mut().item_spacing.y = 0.0;
    area.show_rows(ui, ROW, shown.len(), |ui, range| {
        for r in &shown[range] {
            let allowed = support.privileges_of(r.object.kind);
            row(ui, None, |ui| {
                cell(ui, W_LABEL, &r.label).on_hover_text(kind_label(r.object.kind));
                for p in &columns {
                    let applies = allowed.iter().any(|a| a == p);
                    if privilege_cell(ui, state, r, p, applies) {
                        changed = true;
                    }
                }
            });
        }
    });
    changed
}

/// One cell; returns whether it was clicked (and so changed).
fn privilege_cell(
    ui: &mut egui::Ui,
    state: &mut PrivilegeGridState,
    r: &PrivilegeRow,
    privilege: &str,
    applies: bool,
) -> bool {
    let sense = if applies {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(W_CELL, ROW), sense);
    if !applies {
        return false;
    }
    let current = state.cell(&r.grantee, &r.object, privilege);
    let label = format!("{privilege} on {}", r.label);
    response.widget_info(|| {
        WidgetInfo::selected(
            WidgetType::Checkbox,
            true,
            current != PrivilegeCell::None,
            &label,
        )
    });
    let p = theme::palette(ui.ctx());
    if state.changed(&r.grantee, &r.object, privilege) {
        ui.painter()
            .rect_filled(rect.shrink(1.0), 2.0, change_tint(p.warning));
    } else if response.hovered() {
        ui.painter().rect_filled(rect.shrink(1.0), 2.0, p.hover);
    }
    let center = rect.center();
    match current {
        PrivilegeCell::None => {
            let square = egui::Rect::from_center_size(center, vec2(10.0, 10.0));
            ui.painter().rect_stroke(
                square,
                2.0,
                egui::Stroke::new(1.0, p.border),
                egui::StrokeKind::Inside,
            );
        }
        PrivilegeCell::Granted | PrivilegeCell::Grantable => {
            ui.painter().text(
                center,
                Align2::CENTER_CENTER,
                icons::COMMIT,
                icons::font(ui, 14.0),
                p.success,
            );
            if current == PrivilegeCell::Grantable {
                ui.painter().text(
                    center + vec2(10.0, -4.0),
                    Align2::CENTER_CENTER,
                    icons::ADD,
                    icons::font(ui, 9.0),
                    p.success,
                );
            }
        }
    }
    let response = response.on_hover_text(format!("{label}: {}", current.describe()));
    if response.clicked() {
        state.set(&r.grantee, &r.object, privilege, current.next());
        return true;
    }
    false
}
