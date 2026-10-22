//! Server properties grouped by category (collapsible), with a search over names, values
//! and descriptions. Rows are flattened (headings and properties) and virtualized.

use std::collections::{BTreeMap, HashSet};

use egui::{RichText, TextEdit};
use sqlr_core::ServerProperty;

use super::{ROW, caption, cell, row};
use crate::theme::{self, Spacing, icons};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PropertiesState {
    /// Case-insensitive substring of a property's name, value or description.
    pub search: String,
    /// Categories folded away.
    pub collapsed: HashSet<String>,
}

enum Line<'a> {
    Category(&'a str, usize),
    Property(&'a ServerProperty),
}

const W_NAME: f32 = 280.0;
const W_VALUE: f32 = 220.0;
const W_SOURCE: f32 = 110.0;

/// The category a property is listed under.
fn category(p: &ServerProperty) -> &str {
    p.category.as_deref().unwrap_or("General")
}

pub fn properties_view(ui: &mut egui::Ui, state: &mut PropertiesState, props: &[ServerProperty]) {
    let palette = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        icons::paint(ui, icons::SEARCH, palette.text_muted);
        ui.add(
            TextEdit::singleline(&mut state.search)
                .hint_text("Search properties")
                .desired_width(240.0),
        );
        caption(ui, &format!("{} properties", props.len()));
    });
    let search = state.search.to_lowercase();
    let matches = |p: &ServerProperty| {
        search.is_empty()
            || p.name.to_lowercase().contains(&search)
            || p.value.to_lowercase().contains(&search)
            || p.description
                .as_deref()
                .is_some_and(|d| d.to_lowercase().contains(&search))
    };
    let mut groups: BTreeMap<&str, Vec<&ServerProperty>> = BTreeMap::new();
    for p in props.iter().filter(|p| matches(p)) {
        groups.entry(category(p)).or_default().push(p);
    }
    let mut lines = Vec::new();
    for (name, members) in &groups {
        lines.push(Line::Category(name, members.len()));
        // A search shows every match, folded categories included.
        if search.is_empty() && state.collapsed.contains(*name) {
            continue;
        }
        lines.extend(members.iter().map(|p| Line::Property(p)));
    }
    let mut toggle = None;
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::both()
        .id_salt("server_properties")
        .auto_shrink([false, false])
        .show_rows(ui, ROW, lines.len(), |ui, range| {
            for line in &lines[range] {
                match line {
                    Line::Category(name, n) => row(ui, Some(palette.bg_subtle), |ui| {
                        let open = !state.collapsed.contains(*name) || !search.is_empty();
                        let caret = if open {
                            icons::CARET_DOWN
                        } else {
                            icons::CARET_RIGHT
                        };
                        icons::paint(ui, caret, palette.text_muted);
                        let heading = ui.add(
                            egui::Button::new(RichText::new(*name).strong())
                                .frame_when_inactive(false),
                        );
                        if heading.clicked() {
                            toggle = Some(name.to_string());
                        }
                        caption(ui, &format!("({n})"));
                    }),
                    Line::Property(p) => row(ui, None, |ui| {
                        ui.add_space(Spacing::XL);
                        cell(ui, W_NAME, RichText::new(&p.name).monospace());
                        let value = match &p.unit {
                            Some(unit) => format!("{} {unit}", p.value),
                            None => p.value.clone(),
                        };
                        cell(ui, W_VALUE, RichText::new(value).monospace());
                        cell(ui, W_SOURCE, p.source.as_deref().unwrap_or(""));
                        if let Some(d) = &p.description {
                            ui.label(RichText::new(d).color(palette.text_muted));
                        }
                    }),
                }
            }
        });
    if let Some(name) = toggle
        && !state.collapsed.remove(&name)
    {
        state.collapsed.insert(name);
    }
}
