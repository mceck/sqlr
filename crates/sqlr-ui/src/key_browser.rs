//! The Redis key browser: a match pattern, the keys grouped by prefix with type badges and
//! TTLs, "Load more" while the `SCAN` has more, multi-select delete with a confirmation, and
//! the dialog that creates a key.
//!
//! The widget takes the loaded keys in and returns what the user asked for; the engine
//! scans and deletes. Key names are bytes: shown as UTF-8 when valid, else escaped `\xNN`.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::hash::{Hash, Hasher};

use egui::{Key, RichText, Sense, TextEdit, WidgetInfo, WidgetType, vec2};
use sqlr_core::{KeyInfo, KeyType, display_key};

use crate::theme::{self, Palette, Spacing, icons};

/// Indent of one group level.
const INDENT: f32 = 14.0;
/// Width of the expand arrow column.
const ARROW_WIDTH: f32 = 18.0;
/// Width of the type badge column.
const BADGE_WIDTH: f32 = 40.0;
/// Keys a delete confirmation lists.
const LISTED: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub enum KeyBrowserAction {
    /// The user picked a key: show its value.
    Open(Vec<u8>),
    LoadMore,
    /// Scan this pattern (as typed) from the start.
    SetPattern(String),
    /// Delete these keys (the user confirmed).
    Delete(Vec<Vec<u8>>),
    /// Scan the pattern again from the start.
    Refresh,
    /// The user wants to create a key.
    NewKey,
}

/// Where the groups of the loaded keys are, to detect a changed list cheaply.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    len: usize,
    ptr: usize,
    bytes: usize,
    ends: u64,
    separator: Vec<u8>,
}

fn fingerprint(keys: &[KeyInfo], separator: &[u8]) -> Fingerprint {
    let mut hasher = DefaultHasher::new();
    if let Some(first) = keys.first() {
        first.name.hash(&mut hasher);
    }
    if let Some(last) = keys.last() {
        last.name.hash(&mut hasher);
    }
    Fingerprint {
        len: keys.len(),
        ptr: keys.as_ptr() as usize,
        bytes: keys.iter().map(|k| k.name.len()).sum(),
        ends: hasher.finish(),
        separator: separator.to_vec(),
    }
}

/// The keys under one prefix: the groups below it and the keys that end here.
#[derive(Default)]
struct Group {
    children: BTreeMap<Vec<u8>, Group>,
    /// Indices into the key list, sorted by name.
    keys: Vec<usize>,
    /// Keys in this group and below.
    count: usize,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn build_tree(keys: &[KeyInfo], separator: &[u8]) -> Group {
    let mut root = Group::default();
    for (idx, key) in keys.iter().enumerate() {
        let mut node = &mut root;
        let mut rest = key.name.as_slice();
        while let Some(at) = find(rest, separator) {
            node = node.children.entry(rest[..at].to_vec()).or_default();
            node.count += 1;
            rest = &rest[at + separator.len()..];
        }
        node.keys.push(idx);
    }
    sort_group(&mut root, keys);
    root
}

fn sort_group(group: &mut Group, keys: &[KeyInfo]) {
    group.keys.sort_by(|a, b| keys[*a].name.cmp(&keys[*b].name));
    for child in group.children.values_mut() {
        sort_group(child, keys);
    }
}

/// One visible line of the tree.
enum Row {
    Group {
        /// The group's prefix with the separator: its identity.
        prefix: Vec<u8>,
        name: Vec<u8>,
        count: usize,
        depth: usize,
        open: bool,
    },
    Key {
        idx: usize,
        depth: usize,
        /// Bytes of the prefix to cut from the name for display.
        strip: usize,
    },
}

fn rows(
    group: &Group,
    prefix: &[u8],
    separator: &[u8],
    depth: usize,
    expanded: &HashSet<Vec<u8>>,
    out: &mut Vec<Row>,
) {
    for (name, child) in &group.children {
        let mut path = prefix.to_vec();
        path.extend_from_slice(name);
        path.extend_from_slice(separator);
        let open = expanded.contains(&path);
        out.push(Row::Group {
            prefix: path.clone(),
            name: name.clone(),
            count: child.count,
            depth,
            open,
        });
        if open {
            rows(child, &path, separator, depth + 1, expanded, out);
        }
    }
    for &idx in &group.keys {
        out.push(Row::Key {
            idx,
            depth,
            strip: prefix.len(),
        });
    }
}

#[derive(Default)]
pub struct KeyBrowserState {
    /// The pattern field's text.
    pub pattern: String,
    /// The key shown in the value view (highlighted).
    pub current: Option<Vec<u8>>,
    /// The keys selected for deletion.
    pub selected: BTreeSet<Vec<u8>>,
    expanded: HashSet<Vec<u8>>,
    anchor: Option<Vec<u8>>,
    confirm: Option<Vec<Vec<u8>>>,
    tree: Option<(Fingerprint, Group)>,
}

impl KeyBrowserState {
    /// Opens or closes the group with this prefix (separator included).
    pub fn set_expanded(&mut self, prefix: &[u8], open: bool) {
        if open {
            self.expanded.insert(prefix.to_vec());
        } else {
            self.expanded.remove(prefix);
        }
    }

    pub fn is_expanded(&self, prefix: &[u8]) -> bool {
        self.expanded.contains(prefix)
    }
}

/// `12m`, `3h`, `2d`: the time left in the largest whole unit.
pub fn format_ttl(ms: i64) -> String {
    let s = ms / 1000;
    match s {
        i64::MIN..=0 => "<1s".into(),
        1..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// Names a text field for accessibility (and tests): its label.
pub(crate) fn name_edit(response: &egui::Response, label: &str) {
    response.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, label));
}

/// The color of a type badge.
pub fn badge_color(ty: &KeyType, p: &Palette) -> egui::Color32 {
    match ty {
        KeyType::String => p.icon_table,
        KeyType::Hash => p.icon_view,
        KeyType::List => p.icon_function,
        KeyType::Set => p.icon_key,
        KeyType::ZSet => p.warning,
        KeyType::Stream => p.accent,
        KeyType::Json | KeyType::Other(_) => p.text_muted,
    }
}

/// The short badge text of a type.
pub fn badge_text(ty: &KeyType) -> String {
    match ty {
        KeyType::String => "str".into(),
        KeyType::Hash => "hash".into(),
        KeyType::List => "list".into(),
        KeyType::Set => "set".into(),
        KeyType::ZSet => "zset".into(),
        KeyType::Stream => "strm".into(),
        KeyType::Json => "json".into(),
        KeyType::Other(name) => name.chars().take(4).collect(),
    }
}

/// The type badge: a short word in the type's color.
pub fn type_badge(ui: &mut egui::Ui, ty: &KeyType) {
    let p = theme::palette(ui.ctx());
    let color = badge_color(ty, &p);
    ui.add_sized(
        vec2(BADGE_WIDTH, Spacing::ROW_TREE),
        egui::Label::new(
            RichText::new(badge_text(ty))
                .small()
                .monospace()
                .color(color),
        ),
    );
}

fn delete_label(n: usize) -> String {
    if n == 1 {
        "Delete 1 key".into()
    } else {
        format!("Delete {n} keys")
    }
}

/// Draws the browser into `ui`: the pattern field and buttons, the grouped keys, and the
/// "Load more" footer. `keys` are the loaded keys, `more` says the scan has more.
pub fn key_browser(
    ui: &mut egui::Ui,
    state: &mut KeyBrowserState,
    keys: &[KeyInfo],
    more: bool,
    separator: &str,
) -> Option<KeyBrowserAction> {
    let p = theme::palette(ui.ctx());
    let mut action = None;
    let sep = separator.as_bytes();

    // The pattern, then the buttons.
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 30.0).max(60.0);
        let field = crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut state.pattern)
                .hint_text("Match pattern, e.g. user:*")
                .desired_width(width),
        );
        name_edit(&field, "Key pattern");
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let scan = icons::row_button(ui, icons::SEARCH, "Scan").clicked();
        if enter || scan {
            // A new pattern starts a new list: nothing stays selected.
            state.selected.clear();
            state.anchor = None;
            action = Some(KeyBrowserAction::SetPattern(state.pattern.clone()));
        }
    });
    ui.horizontal(|ui| {
        if icons::row_button(ui, icons::ADD, "New key").clicked() {
            action = Some(KeyBrowserAction::NewKey);
        }
        if icons::row_button(ui, icons::REFRESH, "Refresh").clicked() {
            action = Some(KeyBrowserAction::Refresh);
        }
        let n = state.selected.len();
        if ui
            .add_enabled(n > 0, egui::Button::new(delete_label(n.max(1))))
            .clicked()
        {
            state.confirm = Some(state.selected.iter().cloned().collect());
        }
    });
    ui.add_space(Spacing::XS);
    theme::hairline(ui);

    // The tree is rebuilt only when the key list changed.
    let print = fingerprint(keys, sep);
    let (print, tree) = match state.tree.take() {
        Some((old, tree)) if old == print => (old, tree),
        _ => {
            // Keys that left the list (deleted, expired, another pattern) are no longer
            // selected.
            let names: HashSet<&[u8]> = keys.iter().map(|k| k.name.as_slice()).collect();
            state
                .selected
                .retain(|name| names.contains(name.as_slice()));
            (print, build_tree(keys, sep))
        }
    };
    let mut visible = Vec::new();
    rows(&tree, &[], sep, 0, &state.expanded, &mut visible);
    state.tree = Some((print, tree));

    let footer = Spacing::STATUS + Spacing::S;
    let height = (ui.available_height() - footer).max(Spacing::ROW_TREE);
    let mut toggled: Option<(Vec<u8>, bool)> = None;
    let mut clicked: Option<(usize, egui::Modifiers)> = None;
    let mut delete_group: Option<Vec<u8>> = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(height)
        .show_rows(ui, Spacing::ROW_TREE, visible.len(), |ui, range| {
            for i in range {
                match &visible[i] {
                    Row::Group {
                        prefix,
                        name,
                        count,
                        depth,
                        open,
                    } => {
                        let label = group_name(name);
                        if group_row(ui, &label, *count, *depth, *open, &p, || {
                            delete_group = Some(prefix.clone());
                        }) {
                            toggled = Some((prefix.clone(), !*open));
                        }
                    }
                    Row::Key { idx, depth, strip } => {
                        let key = &keys[*idx];
                        let selected = state.selected.contains(&key.name)
                            || state.current.as_ref() == Some(&key.name);
                        let leaf = display_key(&key.name[*strip..]);
                        let response = key_row(ui, key, &leaf, *depth, selected, &p);
                        if response.clicked() {
                            clicked = Some((i, ui.input(|i| i.modifiers)));
                        }
                        theme::context_menu(&response, |ui| {
                            if icons::menu_item(ui, icons::OPEN, "Open").clicked() {
                                clicked = Some((i, egui::Modifiers::NONE));
                                ui.close();
                            }
                            if icons::menu_item(ui, icons::COPY, "Copy name").clicked() {
                                ui.ctx().copy_text(display_key(&key.name));
                                ui.close();
                            }
                            if icons::menu_item(ui, icons::DELETE, "Delete…").clicked() {
                                state.confirm = Some(vec![key.name.clone()]);
                                ui.close();
                            }
                        });
                    }
                }
            }
        });
    if let Some((prefix, open)) = toggled {
        state.set_expanded(&prefix, open);
    }
    if let Some((i, modifiers)) = clicked
        && let Row::Key { idx, .. } = &visible[i]
    {
        let name = keys[*idx].name.clone();
        if modifiers.shift
            && let Some(anchor) = state.anchor.clone()
            && let Some(from) = visible
                .iter()
                .position(|r| matches!(r, Row::Key { idx, .. } if keys[*idx].name == anchor))
        {
            let (lo, hi) = (from.min(i), from.max(i));
            state.selected.clear();
            for row in &visible[lo..=hi] {
                if let Row::Key { idx, .. } = row {
                    state.selected.insert(keys[*idx].name.clone());
                }
            }
        } else if modifiers.command {
            if !state.selected.remove(&name) {
                state.selected.insert(name.clone());
            }
            state.anchor = Some(name);
        } else {
            state.selected.clear();
            state.selected.insert(name.clone());
            state.current = Some(name.clone());
            state.anchor = Some(name.clone());
            action = Some(KeyBrowserAction::Open(name));
        }
    }
    if let Some(prefix) = delete_group {
        let doomed: Vec<Vec<u8>> = keys
            .iter()
            .filter(|k| k.name.starts_with(&prefix))
            .map(|k| k.name.clone())
            .collect();
        if !doomed.is_empty() {
            state.confirm = Some(doomed);
        }
    }

    // The footer: how much is loaded, and "Load more".
    ui.add_space(Spacing::XS);
    ui.horizontal(|ui| {
        let loaded = if keys.is_empty() {
            "No keys".to_string()
        } else if keys.len() == 1 {
            "1 key loaded".to_string()
        } else {
            format!("{} keys loaded", keys.len())
        };
        ui.label(RichText::new(loaded).small().color(p.text_muted));
        if more && ui.button("Load more").clicked() {
            action = Some(KeyBrowserAction::LoadMore);
        }
    });

    if let Some(doomed) = delete_confirmation(ui.ctx(), state) {
        for name in &doomed {
            state.selected.remove(name);
        }
        action = Some(KeyBrowserAction::Delete(doomed));
    }
    action
}

fn group_name(name: &[u8]) -> String {
    if name.is_empty() {
        "(empty)".into()
    } else {
        display_key(name)
    }
}

/// A group line: arrow, folder, name and key count. `true` when it should toggle.
fn group_row(
    ui: &mut egui::Ui,
    name: &str,
    count: usize,
    depth: usize,
    open: bool,
    p: &Palette,
    mut delete: impl FnMut(),
) -> bool {
    let mut toggle = false;
    ui.horizontal(|ui| {
        ui.set_min_height(Spacing::ROW_TREE);
        ui.add_space(depth as f32 * INDENT);
        let (rect, arrow) =
            ui.allocate_exact_size(vec2(ARROW_WIDTH, Spacing::ROW_TREE), Sense::click());
        let verb = if open { "Collapse" } else { "Expand" };
        arrow.widget_info(|| {
            WidgetInfo::labeled(WidgetType::Button, true, format!("{verb} {name}"))
        });
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            if open {
                icons::CARET_DOWN
            } else {
                icons::CARET_RIGHT
            },
            icons::font(ui, 12.0),
            p.text_muted,
        );
        toggle |= arrow.clicked();
        icons::paint(ui, icons::FOLDER, p.icon_schema);
        let label = ui.selectable_label(false, name);
        toggle |= label.clicked();
        theme::context_menu(&label, |ui| {
            if icons::menu_item(ui, icons::DELETE, "Delete loaded keys…").clicked() {
                delete();
                ui.close();
            }
        });
        ui.label(
            RichText::new(format!("({count})"))
                .small()
                .color(p.text_muted),
        );
    });
    toggle
}

/// A key line: type badge, name, TTL.
fn key_row(
    ui: &mut egui::Ui,
    key: &KeyInfo,
    leaf: &str,
    depth: usize,
    selected: bool,
    p: &Palette,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.set_min_height(Spacing::ROW_TREE);
        ui.add_space(depth as f32 * INDENT + ARROW_WIDTH);
        type_badge(ui, &key.ty);
        let response = ui.selectable_label(selected, leaf);
        if let Some(ttl) = key.ttl_ms {
            ui.label(RichText::new(format_ttl(ttl)).small().color(p.text_muted))
                .on_hover_text("Time to live");
        }
        response
    })
    .inner
}

/// The delete confirmation; the keys once the user confirms.
fn delete_confirmation(ctx: &egui::Context, state: &mut KeyBrowserState) -> Option<Vec<Vec<u8>>> {
    let doomed = state.confirm.clone()?;
    let p = theme::palette(ctx);
    let n = doomed.len();
    let title = if n == 1 { "Delete key" } else { "Delete keys" };
    let dialog = theme::Dialog::new("kv_delete_keys", theme::icons::DELETE, title).width(480.0);
    let answer = theme::confirm(ctx, dialog, "Delete", |ui| {
        ui.label(if n == 1 {
            "Delete this key?".to_string()
        } else {
            format!("Delete these {n} keys?")
        });
        ui.add_space(Spacing::S);
        for name in doomed.iter().take(LISTED) {
            ui.label(
                RichText::new(display_key(name))
                    .monospace()
                    .color(p.text_muted),
            );
        }
        if n > LISTED {
            ui.label(RichText::new(format!("…and {} more", n - LISTED)).color(p.text_muted));
        }
    });
    match answer {
        Some(yes) => {
            state.confirm = None;
            yes.then_some(doomed)
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str]) -> Vec<KeyInfo> {
        names
            .iter()
            .map(|n| KeyInfo::new(*n, KeyType::String))
            .collect()
    }

    #[test]
    fn ttls_use_the_largest_unit() {
        assert_eq!(format_ttl(400), "<1s");
        assert_eq!(format_ttl(1_500), "1s");
        assert_eq!(format_ttl(59_999), "59s");
        assert_eq!(format_ttl(90_000), "1m");
        assert_eq!(format_ttl(3_600_000), "1h");
        assert_eq!(format_ttl(2 * 86_400_000 + 5), "2d");
    }

    #[test]
    fn the_tree_counts_keys_in_each_group() {
        let list = keys(&["a:b:1", "a:b:2", "a:c", "z"]);
        let tree = build_tree(&list, b":");
        assert_eq!(tree.keys.len(), 1);
        let a = &tree.children[b"a".as_slice()];
        assert_eq!(a.count, 3);
        assert_eq!(a.children[b"b".as_slice()].count, 2);
        assert_eq!(a.keys.len(), 1);
        // No separator: a flat list.
        let flat = build_tree(&list, b"");
        assert!(flat.children.is_empty());
        assert_eq!(flat.keys.len(), 4);
    }

    #[test]
    fn leaf_keys_sort_by_name() {
        let list = keys(&["b", "a", "c"]);
        let tree = build_tree(&list, b":");
        let names: Vec<&[u8]> = tree.keys.iter().map(|i| list[*i].name.as_slice()).collect();
        assert_eq!(names, [b"a".as_slice(), b"b", b"c"]);
    }

    #[test]
    fn the_fingerprint_follows_the_list() {
        let a = keys(&["x:1", "x:2"]);
        let b = keys(&["x:1", "x:2", "x:3"]);
        assert_ne!(fingerprint(&a, b":"), fingerprint(&b, b":"));
        assert_ne!(fingerprint(&a, b":"), fingerprint(&a, b"/"));
        assert_eq!(fingerprint(&a, b":"), fingerprint(&a, b":"));
    }
}
