//! The command palette: ⌘P over database objects, ⌘⇧P over commands, snippets and history.
//!
//! The widget only ranks and shows the items it is given and reports which one was chosen; the
//! caller keeps whatever each item stands for in a parallel list.

use egui::{Align, Align2, Id, Key, Layout, Modifiers, RichText, TextEdit, vec2};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::theme::{self, Palette, Spacing, icons};

/// The most fuzzy matches listed; the non-fuzzy items follow them.
pub const PALETTE_ROWS: usize = 50;
const WIDTH: f32 = 600.0;
const TOP: f32 = 60.0;
const MAX_LIST_HEIGHT: f32 = 420.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteMode {
    /// ⌘P: tables, views and other relations of the known catalogs.
    Objects,
    /// ⌘⇧P: app commands, snippets and history.
    Commands,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteGroup {
    Object,
    Command,
    Snippet,
    History,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaletteItem {
    pub label: String,
    pub detail: String,
    pub group: PaletteGroup,
    /// `false`: already filtered by the caller; listed after the fuzzy matches, in order.
    pub fuzzy: bool,
    /// The row's icon; `None` uses the group's.
    pub icon: Option<&'static str>,
    /// The keyboard shortcut, right-aligned in its own column (see
    /// [`crate::keymap::shortcut`]).
    pub shortcut: Option<String>,
}

#[derive(Default)]
pub struct PaletteState {
    /// `None` while the palette is closed.
    pub mode: Option<PaletteMode>,
    pub query: String,
    /// The highlighted row (an index into the rows, not the items).
    selected: usize,
    /// The rows (item indices) for `(query, revision, item count)`.
    ranked: Option<(String, u64, usize, Vec<usize>)>,
    /// Set by `open`: focus the query field and ignore this frame's clicks.
    just_opened: bool,
}

impl PaletteState {
    pub fn open(&mut self, mode: PaletteMode) {
        self.mode = Some(mode);
        self.query.clear();
        self.selected = 0;
        self.ranked = None;
        self.just_opened = true;
    }

    pub fn close(&mut self) {
        self.mode = None;
        self.query.clear();
        self.selected = 0;
        self.ranked = None;
        self.just_opened = false;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaletteOutput {
    None,
    QueryChanged,
    /// The index into `items` of the chosen item. The palette has closed.
    Chosen(usize),
    Closed,
}

/// Indices of the `fuzzy` items matching `query`, best first (ties keep their order), at most
/// `limit`. Case is ignored and accents are normalized unless the query has some. An empty
/// query keeps the first `limit` fuzzy items in order.
pub fn fuzzy_rank(query: &str, items: &[PaletteItem], limit: usize) -> Vec<usize> {
    let fuzzy = items.iter().enumerate().filter(|(_, item)| item.fuzzy);
    if query.trim().is_empty() {
        return fuzzy.map(|(i, _)| i).take(limit).collect();
    }
    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize)> = fuzzy
        .filter_map(|(i, item)| {
            let score = pattern.score(Utf32Str::new(&item.label, &mut buf), &mut matcher)?;
            Some((score, i))
        })
        .collect();
    // Stable: equal scores keep the caller's order.
    scored.sort_by_key(|a| std::cmp::Reverse(a.0));
    scored.into_iter().take(limit).map(|(_, i)| i).collect()
}

/// Draws the open palette (nothing while `state.mode` is `None`). `revision` must change
/// whenever `items` does; the ranking is recomputed only when it or the query changes.
pub fn palette(
    ctx: &egui::Context,
    state: &mut PaletteState,
    items: &[PaletteItem],
    revision: u64,
) -> PaletteOutput {
    if state.mode.is_none() {
        return PaletteOutput::None;
    }
    // Consumed before the query field sees them: a single-line field would act on them too.
    let (up, down, enter, escape) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Escape),
        )
    });
    if escape {
        state.close();
        return PaletteOutput::Closed;
    }

    let mut listed = rows(state, items, revision).to_vec();
    if down && state.selected + 1 < listed.len() {
        state.selected += 1;
    }
    if up {
        state.selected = state.selected.saturating_sub(1);
    }
    state.selected = state.selected.min(listed.len().saturating_sub(1));
    if enter && let Some(&chosen) = listed.get(state.selected) {
        state.close();
        return PaletteOutput::Chosen(chosen);
    }

    let mut output = PaletteOutput::None;
    let just_opened = std::mem::take(&mut state.just_opened);
    let query_id = Id::new("sqlr_palette_query");
    let colors = theme::palette(ctx);
    let area = egui::Area::new(Id::new("sqlr_palette"))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_TOP, vec2(0.0, TOP))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .corner_radius(Spacing::RADIUS_M)
                .inner_margin(Spacing::M)
                .show(ui, |ui| {
                    ui.set_width(WIDTH);
                    // The placeholder names the palette's own shortcut, so it is learned here.
                    let (hint, action) = match state.mode {
                        Some(PaletteMode::Objects) => {
                            ("Go to a table or view", sqlr_core::ActionId::OpenObject)
                        }
                        _ => (
                            "Run a command, insert a snippet, search history",
                            sqlr_core::ActionId::Commands,
                        ),
                    };
                    let hint = crate::keymap::tooltip(ctx, hint, action);
                    let field = crate::text_menu::text_edit(
                        ui,
                        TextEdit::singleline(&mut state.query)
                            .id(query_id)
                            .hint_text(hint)
                            .desired_width(f32::INFINITY),
                    );
                    if !field.has_focus() {
                        field.request_focus();
                    }
                    if field.changed() {
                        state.selected = 0;
                        output = PaletteOutput::QueryChanged;
                        // Ranked now, so this frame lists the rows of the query just typed.
                        listed = rows(state, items, revision).to_vec();
                    }
                    ui.separator();
                    if listed.is_empty() {
                        ui.label(RichText::new("No matches").color(colors.text_muted));
                        return;
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("sqlr_palette_rows")
                        .max_height(MAX_LIST_HEIGHT)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (row, &index) in listed.iter().enumerate() {
                                let item = &items[index];
                                let selected = row == state.selected;
                                let response = theme::list_row(
                                    ui,
                                    Id::new(("sqlr_palette_row", index)),
                                    selected,
                                    Spacing::ROW_TREE,
                                    egui::Sense::click(),
                                    |ui| item_row(ui, item, &colors),
                                )
                                .response;
                                if selected && (up || down) {
                                    response.scroll_to_me(None);
                                }
                                if response.clicked() {
                                    output = PaletteOutput::Chosen(index);
                                }
                            }
                        });
                });
        });
    match output {
        PaletteOutput::Chosen(_) => state.close(),
        _ if !just_opened && area.response.clicked_elsewhere() => {
            state.close();
            output = PaletteOutput::Closed;
        }
        _ => {}
    }
    output
}

/// One row: icon, label and detail on the left, the shortcut right-aligned in its own
/// column.
fn item_row(ui: &mut egui::Ui, item: &PaletteItem, colors: &Palette) {
    ui.add_space(Spacing::XS);
    let (icon, color) = group_icon(item.group);
    icons::paint(ui, item.icon.unwrap_or(icon), color(colors));
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        // Clear of the overlaid scroll bar.
        ui.add_space(Spacing::L);
        if let Some(keys) = &item.shortcut {
            ui.add(
                egui::Label::new(RichText::new(keys).color(colors.text_muted)).selectable(false),
            );
            ui.add_space(Spacing::L);
        }
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add(egui::Label::new(&item.label).selectable(false).truncate());
            if !item.detail.is_empty() {
                ui.add(
                    egui::Label::new(RichText::new(&item.detail).small().color(colors.text_muted))
                        .selectable(false)
                        .truncate(),
                );
            }
        });
    });
}

/// The icon of a palette group and its color.
fn group_icon(group: PaletteGroup) -> (&'static str, fn(&Palette) -> egui::Color32) {
    match group {
        PaletteGroup::Object => (icons::TABLE, |p| p.icon_table),
        PaletteGroup::Command => (icons::COMMAND, |p| p.text_muted),
        PaletteGroup::Snippet => (icons::SNIPPET, |p| p.icon_function),
        PaletteGroup::History => (icons::HISTORY, |p| p.text_muted),
    }
}

/// The rows for this frame: the fuzzy matches, then every non-fuzzy item.
fn rows<'s>(state: &'s mut PaletteState, items: &[PaletteItem], revision: u64) -> &'s [usize] {
    let fresh = state
        .ranked
        .as_ref()
        .is_some_and(|(q, rev, n, _)| *q == state.query && *rev == revision && *n == items.len());
    if !fresh {
        let mut rows = fuzzy_rank(&state.query, items, PALETTE_ROWS);
        rows.extend(
            items
                .iter()
                .enumerate()
                .filter(|(_, item)| !item.fuzzy)
                .map(|(i, _)| i),
        );
        state.ranked = Some((state.query.clone(), revision, items.len(), rows));
    }
    state.ranked.as_ref().map_or(&[], |(_, _, _, rows)| rows)
}
