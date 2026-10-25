//! The snippets panel (folders split on `/`, scoped to the active connection) and the snippet
//! editing dialog.

use std::collections::{BTreeMap, HashSet};

use egui::{
    Align2, ComboBox, Frame, Id, Key, Margin, RichText, Sense, TextEdit, WidgetInfo, WidgetType,
    vec2,
};
use sqlr_core::Snippet;

use crate::theme::{self, Spacing, icons};

const INDENT: f32 = Spacing::XL;
const ARROW_WIDTH: f32 = 16.0;

/// A snippet being created (`id: None`) or edited.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnippetDraft {
    pub id: Option<String>,
    pub name: String,
    pub folder: String,
    pub conn_id: Option<String>,
    pub sql: String,
}

impl SnippetDraft {
    pub fn from_snippet(s: &Snippet) -> Self {
        Self {
            id: Some(s.id.clone()),
            name: s.name.clone(),
            folder: s.folder.clone(),
            conn_id: s.conn_id.clone(),
            sql: s.sql.clone(),
        }
    }
}

/// The longest suggested name, in characters, before it is clipped.
const SUGGESTED_NAME_CHARS: usize = 48;

/// A starting name for a snippet, taken from the first line of its SQL that is not blank or
/// a comment. Whitespace is collapsed and long lines are clipped, so the name stays readable
/// in the panel and the palette.
pub fn suggest_snippet_name(sql: &str) -> String {
    let line = first_content_line(sql);
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return "Snippet".to_string();
    }
    clip_name(&collapsed)
}

/// The first line of `sql` outside leading `--`, `//`, `#` and `/* … */` comments.
fn first_content_line(sql: &str) -> &str {
    let mut rest = sql;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("--").or_else(|| rest.strip_prefix("//")) {
            rest = after.split_once('\n').map_or("", |(_, rest)| rest);
        } else if let Some(after) = rest.strip_prefix('#') {
            rest = after.split_once('\n').map_or("", |(_, rest)| rest);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, rest)| rest);
        } else {
            return rest.lines().next().unwrap_or("").trim();
        }
    }
}

/// `name`, shortened to [`SUGGESTED_NAME_CHARS`] at a word boundary when one is close enough.
fn clip_name(name: &str) -> String {
    if name.chars().count() <= SUGGESTED_NAME_CHARS {
        return name.to_string();
    }
    let mut clipped: String = name.chars().take(SUGGESTED_NAME_CHARS - 1).collect();
    if let Some(space) = clipped.rfind(' ')
        && space >= SUGGESTED_NAME_CHARS / 2
    {
        clipped.truncate(space);
    }
    clipped.push('…');
    clipped
}

#[derive(Default)]
pub struct SnippetsState {
    /// Case-insensitive substring filter over snippet names.
    pub filter: String,
    /// Open folders, by their full `/`-joined path.
    expanded: HashSet<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SnippetAction {
    /// Insert this SQL at the active editor's carets.
    Insert(String),
    OpenInNewTab(Snippet),
    Edit(SnippetDraft),
    /// The snippet id.
    Delete(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftResult {
    Open,
    Save,
    Cancel,
}

/// A folder level: subfolders by name, then snippets.
#[derive(Default)]
struct Folder<'a> {
    folders: BTreeMap<&'a str, Folder<'a>>,
    snippets: Vec<&'a Snippet>,
}

enum Row<'a> {
    Folder {
        path: String,
        name: &'a str,
        depth: usize,
        open: bool,
    },
    Snippet {
        snippet: &'a Snippet,
        depth: usize,
    },
}

fn flatten<'a>(
    folder: &Folder<'a>,
    prefix: &str,
    depth: usize,
    state: &SnippetsState,
    filtering: bool,
    rows: &mut Vec<Row<'a>>,
) {
    for (name, sub) in &folder.folders {
        let path = if prefix.is_empty() {
            (*name).to_string()
        } else {
            format!("{prefix}/{name}")
        };
        // A filter shows every folder holding a match.
        let open = filtering || state.expanded.contains(&path);
        rows.push(Row::Folder {
            path: path.clone(),
            name,
            depth,
            open,
        });
        if open {
            flatten(sub, &path, depth + 1, state, filtering, rows);
        }
    }
    for snippet in &folder.snippets {
        rows.push(Row::Snippet { snippet, depth });
    }
}

/// Global snippets plus those of `active_conn`.
pub fn snippets_panel(
    ui: &mut egui::Ui,
    state: &mut SnippetsState,
    snippets: &[Snippet],
    active_conn: Option<&str>,
) -> Vec<SnippetAction> {
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        if icons::icon_button(ui, icons::ADD, None, "New snippet").clicked() {
            actions.push(SnippetAction::Edit(SnippetDraft::default()));
        }
        crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut state.filter)
                .hint_text("Filter")
                .desired_width(f32::INFINITY),
        );
    });
    ui.add_space(2.0);

    let filter = state.filter.to_lowercase();
    let mut visible: Vec<&Snippet> = snippets
        .iter()
        .filter(|s| s.conn_id.is_none() || s.conn_id.as_deref() == active_conn)
        .filter(|s| filter.is_empty() || s.name.to_lowercase().contains(&filter))
        .collect();
    visible.sort_by_cached_key(|s| s.name.to_lowercase());
    if visible.is_empty() {
        let text = if snippets.is_empty() {
            "No snippets yet"
        } else {
            "No matching snippets"
        };
        let description = if snippets.is_empty() {
            "Save SQL you use often. Choose + to create a snippet, then open it or insert it into a query."
        } else {
            "Try a different filter or switch to the connection this snippet belongs to."
        };
        theme::empty_panel(ui, icons::SNIPPET, text, description);
        return actions;
    }
    let mut root = Folder::default();
    for s in visible {
        let mut folder = &mut root;
        for segment in s.folder.split('/').filter(|p| !p.is_empty()) {
            folder = folder.folders.entry(segment).or_default();
        }
        folder.snippets.push(s);
    }
    let mut rows = Vec::new();
    flatten(&root, "", 0, state, !filter.is_empty(), &mut rows);

    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical()
        .id_salt("snippets_scroll")
        .auto_shrink(false)
        .show_rows(ui, Spacing::ROW_TREE, rows.len(), |ui, range| {
            for row in &rows[range] {
                match row {
                    Row::Folder {
                        path,
                        name,
                        depth,
                        open,
                    } => {
                        if folder_row(ui, name, *depth, *open) {
                            if *open {
                                state.expanded.remove(path);
                            } else {
                                state.expanded.insert(path.clone());
                            }
                        }
                    }
                    Row::Snippet { snippet, depth } => {
                        snippet_row(ui, snippet, *depth, &mut actions);
                    }
                }
            }
        });
    actions
}

/// Returns whether the folder was toggled.
fn folder_row(ui: &mut egui::Ui, name: &str, depth: usize, open: bool) -> bool {
    let mut toggle = false;
    let palette = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        ui.add_space(depth as f32 * INDENT);
        let (rect, arrow) =
            ui.allocate_exact_size(vec2(ARROW_WIDTH, Spacing::ROW_TREE), Sense::click());
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            if open {
                icons::CARET_DOWN
            } else {
                icons::CARET_RIGHT
            },
            icons::font(ui, 12.0),
            palette.text_muted,
        );
        let verb = if open { "Collapse" } else { "Expand" };
        arrow.widget_info(|| {
            WidgetInfo::labeled(WidgetType::Button, true, format!("{verb} {name}"))
        });
        toggle |= arrow.clicked();
        icons::paint(ui, icons::FOLDER, palette.icon_schema);
        toggle |= ui
            .selectable_label(false, RichText::new(name).color(palette.text_muted))
            .clicked();
    });
    toggle
}

fn snippet_row(
    ui: &mut egui::Ui,
    snippet: &Snippet,
    depth: usize,
    actions: &mut Vec<SnippetAction>,
) {
    let palette = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        ui.add_space(depth as f32 * INDENT + ARROW_WIDTH + Spacing::S);
        icons::paint(ui, icons::SNIPPET, palette.icon_function);
        let response = ui
            .selectable_label(false, &snippet.name)
            .on_hover_text(&snippet.sql);
        if response.double_clicked() {
            actions.push(SnippetAction::Insert(snippet.sql.clone()));
        }
        theme::context_menu(&response, |ui| {
            if icons::menu_item(ui, icons::INSERT, "Insert").clicked() {
                actions.push(SnippetAction::Insert(snippet.sql.clone()));
                ui.close();
            }
            if icons::menu_item(ui, icons::OPEN, "Open in new tab").clicked() {
                actions.push(SnippetAction::OpenInNewTab(snippet.clone()));
                ui.close();
            }
            if icons::menu_item(ui, icons::EDIT, "Edit…").clicked() {
                actions.push(SnippetAction::Edit(SnippetDraft::from_snippet(snippet)));
                ui.close();
            }
            if icons::menu_item(ui, icons::DELETE, "Delete").clicked() {
                actions.push(SnippetAction::Delete(snippet.id.clone()));
                ui.close();
            }
        });
    });
}

/// Edits `draft`; `conns` are `(id, name)` pairs for the scope picker. Save stays disabled
/// while the name is empty.
pub fn snippet_dialog(
    ctx: &egui::Context,
    draft: &mut SnippetDraft,
    conns: &[(String, String)],
) -> DraftResult {
    let mut result = DraftResult::Open;
    let mut open = true;
    let id = Id::new("snippet_dialog");
    // Not drawn in the previous frame: the dialog has just opened, so focus its name field.
    let frame = ctx.cumulative_frame_nr();
    let drawn_at = id.with("drawn_at");
    let previous = ctx.data_mut(|d| d.get_temp::<u64>(drawn_at));
    let just_opened = previous.is_none_or(|f| f.wrapping_add(1) != frame);
    ctx.data_mut(|d| d.insert_temp(drawn_at, frame));
    let title = if draft.id.is_some() {
        "Edit snippet"
    } else {
        "Save snippet"
    };
    let subtitle = if draft.id.is_some() {
        "Update the saved SQL and where it belongs"
    } else {
        "Keep this SQL for reuse"
    };
    let mut close = false;
    egui::Window::new(title)
        .id(id)
        .open(&mut open)
        .title_bar(false)
        .collapsible(false)
        .resizable(true)
        .default_width(480.0)
        .frame(Frame::window(&ctx.global_style()).inner_margin(Margin::ZERO))
        .show(ctx, |ui| {
            if theme::dialog_header(ui, icons::SNIPPET, title, Some(subtitle)) {
                close = true;
            }
            Frame::new()
                .inner_margin(Margin::symmetric(16, 14))
                .show(ui, |ui| {
                    theme::field_label(ui, "Name");
                    let name = crate::text_menu::text_edit(
                        ui,
                        TextEdit::singleline(&mut draft.name).desired_width(f32::INFINITY),
                    );
                    if just_opened {
                        name.request_focus();
                        // A suggested name is a starting point: typing replaces it.
                        if draft.id.is_none() && !draft.name.is_empty() {
                            select_all(ctx, name.id, draft.name.chars().count());
                        }
                    }
                    theme::field_label(ui, "Folder");
                    crate::text_menu::text_edit(
                        ui,
                        TextEdit::singleline(&mut draft.folder)
                            .hint_text("e.g. reports/monthly")
                            .desired_width(f32::INFINITY),
                    );
                    theme::field_label(ui, "Scope");
                    let current = draft
                        .conn_id
                        .as_ref()
                        .map(|id| {
                            conns
                                .iter()
                                .find(|(i, _)| i == id)
                                .map_or(id.as_str(), |(_, name)| name.as_str())
                        })
                        .unwrap_or("Global");
                    ComboBox::from_id_salt("snippet_scope")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(draft.conn_id.is_none(), "Global")
                                .clicked()
                            {
                                draft.conn_id = None;
                            }
                            for (id, name) in conns {
                                let selected = draft.conn_id.as_ref() == Some(id);
                                if ui.selectable_label(selected, name).clicked() {
                                    draft.conn_id = Some(id.clone());
                                }
                            }
                        });
                    theme::field_label(ui, "SQL");
                    egui::ScrollArea::vertical()
                        .id_salt("snippet_sql")
                        .max_height(240.0)
                        .show(ui, |ui| {
                            crate::text_menu::text_edit(
                                ui,
                                TextEdit::multiline(&mut draft.sql)
                                    .code_editor()
                                    .desired_rows(8)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    ui.add_space(Spacing::L);
                    ui.horizontal(|ui| {
                        let named = !draft.name.trim().is_empty();
                        let save = theme::primary_button_state(ui.ctx(), named, "Save");
                        if ui.add_enabled(named, save).clicked() {
                            result = DraftResult::Save;
                        }
                        if ui.button("Cancel").clicked() {
                            result = DraftResult::Cancel;
                        }
                    });
                });
        });
    if close {
        open = false;
    }
    if result == DraftResult::Open
        && (!open || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)))
    {
        result = DraftResult::Cancel;
    }
    result
}

/// Selects the whole text of the field `id`, so typing replaces it.
fn select_all(ctx: &egui::Context, id: Id, chars: usize) {
    let mut state = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(chars),
        )));
    state.store(ctx, id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggested_names_come_from_the_first_content_line() {
        assert_eq!(suggest_snippet_name("  SELECT *\nFROM users"), "SELECT *");
        assert_eq!(
            suggest_snippet_name("-- a note\n// another\n\nselect 1;"),
            "select 1;"
        );
        assert_eq!(
            suggest_snippet_name("/* header */  SELECT  1\nFROM t"),
            "SELECT 1"
        );
        assert_eq!(suggest_snippet_name("/* only a comment */"), "Snippet");
        assert_eq!(suggest_snippet_name("   \n\t"), "Snippet");
    }

    #[test]
    fn suggested_names_are_clipped_at_a_word_boundary() {
        let long = format!("SELECT {}", "column_name ".repeat(10));
        let name = suggest_snippet_name(&long);
        assert!(name.chars().count() <= SUGGESTED_NAME_CHARS, "{name:?}");
        assert!(name.ends_with('…'), "{name:?}");
        assert!(!name.ends_with(" …"), "{name:?}");
    }
}
