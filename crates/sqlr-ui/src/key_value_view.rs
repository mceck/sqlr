//! The Redis value view: a key's header (type, size, TTL, rename, delete) and its value as
//! a per-type table or editor. A string shows as text, JSON or hex; a hash, list, set, sorted
//! set or stream as rows with inline editing. Only the page the engine loaded is shown;
//! "Load more" asks for the next one.

use std::ops::Range;

use egui::{Key, RichText, TextEdit, vec2};
use sqlr_core::{
    KeyInfo, KeyType, KvEdit, KvPage, display_key, parse_key_name, pretty_json, quote_bytes,
};

use crate::key_browser::{format_ttl, name_edit, type_badge};
use crate::theme::{self, Palette, Spacing, icons};

/// A string larger than this is shown read-only (editing it would stall the frame).
const EDIT_MAX: usize = 64 * 1024;
/// An element larger than this cannot be edited inline.
const CELL_EDIT_MAX: usize = 64 * 1024;
/// Characters of an element shown in a table cell.
const CELL_CHARS: usize = 200;
/// Characters of one visual line of a long text.
const WRAP_CHARS: usize = 160;
/// Bytes of a string the hex view shows.
const HEX_MAX: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum KvViewAction {
    /// Load the next page of the value.
    LoadMore,
    Edit(KvEdit),
    /// Load the key again from its first page.
    Refresh,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum StringMode {
    #[default]
    Text,
    Json,
    Hex,
}

/// An element being edited inline.
struct Editing {
    row: usize,
    buffer: String,
    /// Put the caret in the field on its first frame.
    focus: bool,
}

/// What a read-only text view shows, cut into visual lines.
struct TextRows {
    print: (usize, usize, StringMode),
    text: String,
    rows: Vec<Range<usize>>,
}

#[derive(Default)]
pub struct KvViewState {
    /// The key the buffers belong to.
    key: Vec<u8>,
    /// Identity of the page the buffers were made from: its address and length.
    page_print: (usize, usize),
    mode: StringMode,
    mode_set: bool,
    text: String,
    original: String,
    ttl: String,
    rename: Option<String>,
    confirm_delete: bool,
    editing: Option<Editing>,
    add_a: String,
    add_b: String,
    error: Option<String>,
    rows: Option<TextRows>,
    /// Save the unsaved edit on the next frame, as its Save button would.
    save_requested: bool,
}

impl KvViewState {
    /// Whether an edit is not saved yet: an open inline element editor, or a string value
    /// that differs from the one loaded.
    pub fn has_unsaved_edit(&self) -> bool {
        self.editing.is_some() || self.text != self.original
    }

    /// Saves the unsaved edit when the view next draws, as its Save button (or Enter in an
    /// element editor) would.
    pub fn request_save(&mut self) {
        self.save_requested = self.has_unsaved_edit();
    }

    /// Drops the unsaved edit, as Cancel or Revert would.
    pub fn discard_edit(&mut self) {
        self.editing = None;
        self.text = self.original.clone();
    }

    /// Forgets everything typed for another key.
    fn focus(&mut self, key: &[u8]) {
        if self.key != key {
            *self = KvViewState {
                key: key.to_vec(),
                ..Default::default()
            };
        }
    }
}

/// `1.5 MiB`, `12 B`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A TTL typed by the user, in milliseconds: `90` (seconds), `500ms`, `45s`, `5m`, `2h`,
/// `1d`. `None` for text that is not a positive duration.
pub fn parse_ttl(text: &str) -> Option<i64> {
    let text = text.trim().to_ascii_lowercase();
    let (number, unit) = match text.find(|c: char| !c.is_ascii_digit() && c != '.') {
        Some(at) => text.split_at(at),
        None => (text.as_str(), "s"),
    };
    let factor: f64 = match unit.trim() {
        "ms" => 1.0,
        "s" | "" => 1_000.0,
        "m" => 60_000.0,
        "h" => 3_600_000.0,
        "d" => 86_400_000.0,
        _ => return None,
    };
    let ms = number.parse::<f64>().ok()? * factor;
    (ms >= 1.0 && ms < i64::MAX as f64).then_some(ms.round() as i64)
}

/// An element as a table cell shows it: escaped, cut to a line.
fn cell_text(bytes: &[u8]) -> String {
    let shown = display_key(bytes);
    match shown.char_indices().nth(CELL_CHARS) {
        Some((cut, _)) => format!("{}…", &shown[..cut]),
        None => shown,
    }
}

/// `text` split at line breaks, then every `WRAP_CHARS` characters.
fn wrap_rows(text: &str) -> Vec<Range<usize>> {
    let mut rows = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let line_end = start + line.len();
        let body_end = start + line.trim_end_matches(['\n', '\r']).len();
        let mut at = start;
        loop {
            let rest = &text[at..body_end];
            match rest.char_indices().nth(WRAP_CHARS) {
                Some((cut, _)) => {
                    rows.push(at..at + cut);
                    at += cut;
                }
                None => {
                    rows.push(at..body_end);
                    break;
                }
            }
        }
        start = line_end;
    }
    rows
}

/// One line of a hex dump: `offset  16 hex bytes  |ASCII|`.
fn hex_row(bytes: &[u8], row: usize) -> String {
    use std::fmt::Write;
    let line = &bytes[row * 16..(row * 16 + 16).min(bytes.len())];
    let mut out = format!("{:08x}  ", row * 16);
    for i in 0..16 {
        match line.get(i) {
            Some(b) => {
                let _ = write!(out, "{b:02x} ");
            }
            None => out.push_str("   "),
        }
        if i == 7 {
            out.push(' ');
        }
    }
    out.push_str(" |");
    out.extend(line.iter().map(|&b| {
        if b.is_ascii_graphic() || b == b' ' {
            char::from(b)
        } else {
            '.'
        }
    }));
    out.push('|');
    out
}

/// A single-line field with an accessible name.
fn field(
    ui: &mut egui::Ui,
    text: &mut String,
    label: &str,
    hint: &str,
    width: f32,
) -> egui::Response {
    let response = crate::text_menu::text_edit(
        ui,
        TextEdit::singleline(text)
            .hint_text(hint)
            .desired_width(width),
    );
    name_edit(&response, label);
    response
}

fn unit_of(ty: &KeyType) -> &'static str {
    match ty {
        KeyType::String => "bytes",
        KeyType::Hash => "fields",
        KeyType::List => "items",
        KeyType::Set | KeyType::ZSet => "members",
        KeyType::Stream => "entries",
        KeyType::Json | KeyType::Other(_) => "",
    }
}

/// Draws the key's header and value. `page` is the part of the value loaded so far.
pub fn key_value_view(
    ui: &mut egui::Ui,
    state: &mut KvViewState,
    key: &KeyInfo,
    page: Option<&KvPage>,
) -> Option<KvViewAction> {
    state.focus(&key.name);
    let p = theme::palette(ui.ctx());
    let mut action = header(ui, state, key, &p);
    ui.add_space(Spacing::S);
    theme::hairline(ui);
    ui.add_space(Spacing::S);
    if let Some(error) = state.error.clone() {
        ui.colored_label(p.error, error);
    }
    let Some(page) = page else {
        let text = match key.ty {
            KeyType::Json | KeyType::Other(_) => format!(
                "Values of type {} have no viewer. Use the console, for example JSON.GET {}.",
                key.ty.label(),
                display_key(&key.name)
            ),
            _ => "Nothing loaded.".to_string(),
        };
        theme::hint(ui, &text);
        state.save_requested = false;
        return action;
    };
    if let Some(edit) = body(ui, state, page, &p, &mut action) {
        state.editing = None;
        state.error = None;
        action = Some(KvViewAction::Edit(edit));
    }
    // A request the page could not answer (the edited row scrolled away) is dropped.
    state.save_requested = false;
    action
}

fn header(
    ui: &mut egui::Ui,
    state: &mut KvViewState,
    key: &KeyInfo,
    p: &Palette,
) -> Option<KvViewAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        type_badge(ui, &key.ty);
        ui.label(RichText::new(display_key(&key.name)).monospace().strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icons::row_button(ui, icons::REFRESH, "Refresh").clicked() {
                action = Some(KvViewAction::Refresh);
            }
        });
    });
    let mut parts = Vec::new();
    if let Some(n) = key.length {
        parts.push(format!("{n} {}", unit_of(&key.ty)));
    }
    if let Some(size) = key.size {
        parts.push(format!("{} in memory", format_size(size)));
    }
    parts.push(match key.ttl_ms {
        Some(ms) => format!("expires in {}", format_ttl(ms)),
        None => "no expiry".to_string(),
    });
    ui.label(
        RichText::new(parts.join("  ·  "))
            .small()
            .color(p.text_muted),
    );
    ui.add_space(Spacing::XS);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("TTL").small().color(p.text_muted));
        let ttl = field(ui, &mut state.ttl, "TTL", "90, 5m, 2h, 1d", 110.0);
        let apply = ui.button("Set TTL").clicked()
            || (ttl.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)));
        if apply && !state.ttl.trim().is_empty() {
            match parse_ttl(&state.ttl) {
                Some(ms) => {
                    state.error = None;
                    action = Some(KvViewAction::Edit(KvEdit::Expire(Some(ms))));
                }
                None => {
                    state.error =
                        Some("The TTL is a positive duration: 90, 500ms, 5m, 2h, 1d".into());
                }
            }
        }
        if ui
            .add_enabled(key.ttl_ms.is_some(), egui::Button::new("Persist"))
            .clicked()
        {
            action = Some(KvViewAction::Edit(KvEdit::Expire(None)));
        }
        ui.separator();
        if ui.button("Rename").clicked() {
            state.rename = match state.rename {
                Some(_) => None,
                None => Some(quote_bytes(&key.name)),
            };
        }
        let delete = egui::Button::new(RichText::new("Delete key").color(p.error));
        if ui.add(delete).clicked() {
            state.confirm_delete = true;
        }
    });
    if let Some(name) = state.rename.as_mut() {
        let mut done = false;
        ui.horizontal(|ui| {
            let response = field(ui, name, "New name", "New key name", 240.0);
            if ui.button("Rename to").clicked()
                || (response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))
            {
                match parse_key_name(name) {
                    Ok(new) if new.is_empty() => state.error = Some("Enter a key name".into()),
                    Ok(new) => {
                        done = true;
                        // An unchanged name is no rename.
                        if new != key.name {
                            state.error = None;
                            action = Some(KvViewAction::Edit(KvEdit::Rename(new)));
                        }
                    }
                    Err(e) => state.error = Some(e.to_string()),
                }
            }
            if ui.button("Cancel").clicked() {
                done = true;
            }
        });
        if done {
            state.rename = None;
        }
    }
    if state.confirm_delete {
        let dialog =
            theme::Dialog::new("kv_delete_key", theme::icons::DELETE, "Delete key").width(480.0);
        let answer = theme::confirm(ui.ctx(), dialog, "Delete", |ui| {
            ui.label("Delete this key?");
            ui.label(
                RichText::new(display_key(&key.name))
                    .monospace()
                    .color(p.text_muted),
            );
        });
        if let Some(yes) = answer {
            state.confirm_delete = false;
            if yes {
                action = Some(KvViewAction::Edit(KvEdit::Delete));
            }
        }
    }
    action
}

fn body(
    ui: &mut egui::Ui,
    state: &mut KvViewState,
    page: &KvPage,
    p: &Palette,
    action: &mut Option<KvViewAction>,
) -> Option<KvEdit> {
    let mut edit = None;
    let mut more = false;
    match page {
        KvPage::String { bytes, total_len } => {
            edit = string_body(ui, state, bytes, *total_len, p, &mut more);
        }
        KvPage::Hash { fields, .. } => {
            table(
                ui,
                p,
                "hash",
                &[("Field", 200.0), ("Value", 0.0)],
                fields.len(),
                |ui, i| {
                    let (field, value) = &fields[i];
                    let name = cell_text(field);
                    cell(ui, 200.0, &name);
                    if let Some(text) =
                        edit_field(ui, &mut state.editing, i, "Value", state.save_requested)
                    {
                        edit = Some(KvEdit::HashSet(field.clone(), text.into_bytes()));
                        return;
                    }
                    if state.editing.as_ref().is_none_or(|e| e.row != i) {
                        cell(ui, value_width(ui), &cell_text(value));
                        let (begin, remove) = row_buttons(ui, &name, editable(value), "Remove");
                        if begin {
                            begin_edit(&mut state.editing, i, value);
                        }
                        if remove {
                            edit = Some(KvEdit::HashDel(field.clone()));
                        }
                    }
                },
            );
            let new = [("New field", "Field"), ("New value", "Value")];
            if let Some([f, v]) = add_row(ui, state, new, "Add") {
                edit = Some(KvEdit::HashSet(f.into_bytes(), v.into_bytes()));
            }
        }
        KvPage::List { items, start, .. } => {
            table(
                ui,
                p,
                "list",
                &[("Index", 70.0), ("Value", 0.0)],
                items.len(),
                |ui, i| {
                    let index = *start + i as i64;
                    let value = &items[i];
                    cell(ui, 70.0, &index.to_string());
                    if let Some(text) =
                        edit_field(ui, &mut state.editing, i, "Value", state.save_requested)
                    {
                        edit = Some(KvEdit::ListSet(index, text.into_bytes()));
                        return;
                    }
                    if state.editing.as_ref().is_none_or(|e| e.row != i) {
                        cell(ui, value_width(ui), &cell_text(value));
                        let (begin, remove) =
                            row_buttons(ui, &index.to_string(), editable(value), "Remove");
                        if begin {
                            begin_edit(&mut state.editing, i, value);
                        }
                        if remove {
                            edit = Some(KvEdit::ListRem {
                                index,
                                value: value.clone(),
                            });
                        }
                    }
                },
            );
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 220.0).max(80.0);
                field(ui, &mut state.add_a, "New value", "Value", width);
                let value = state.add_a.clone().into_bytes();
                for (label, left) in [("Push left", true), ("Push right", false)] {
                    if ui.button(label).clicked() {
                        edit = Some(KvEdit::ListPush {
                            left,
                            value: value.clone(),
                        });
                        state.add_a.clear();
                    }
                }
            });
        }
        KvPage::Set { members, .. } => {
            table(ui, p, "set", &[("Member", 0.0)], members.len(), |ui, i| {
                let member = &members[i];
                let name = cell_text(member);
                cell(ui, value_width(ui), &name);
                if icons::row_button(ui, icons::DELETE, &format!("Remove {name}")).clicked() {
                    edit = Some(KvEdit::SetRem(member.clone()));
                }
            });
            if let Some([m]) = add_row(ui, state, [("New member", "Member")], "Add") {
                edit = Some(KvEdit::SetAdd(m.into_bytes()));
            }
        }
        KvPage::ZSet { members, .. } => {
            table(
                ui,
                p,
                "zset",
                &[("Member", 220.0), ("Score", 0.0)],
                members.len(),
                |ui, i| {
                    let (member, score) = &members[i];
                    let name = cell_text(member);
                    cell(ui, 220.0, &name);
                    if let Some(text) =
                        edit_field(ui, &mut state.editing, i, "Score", state.save_requested)
                    {
                        match text.trim().parse::<f64>() {
                            Ok(score) if !score.is_nan() => {
                                edit = Some(KvEdit::ZAdd(member.clone(), score));
                            }
                            _ => state.error = Some("The score must be a number".into()),
                        }
                        return;
                    }
                    if state.editing.as_ref().is_none_or(|e| e.row != i) {
                        cell(ui, value_width(ui), &score.to_string());
                        let (begin, remove) = row_buttons(ui, &name, Ok(()), "Remove");
                        if begin {
                            state.editing = Some(Editing {
                                row: i,
                                buffer: score.to_string(),
                                focus: true,
                            });
                        }
                        if remove {
                            edit = Some(KvEdit::ZRem(member.clone()));
                        }
                    }
                },
            );
            let new = [("New member", "Member"), ("New score", "Score")];
            if let Some([m, s]) = add_row(ui, state, new, "Add") {
                match s.trim().parse::<f64>() {
                    Ok(score) if !score.is_nan() => {
                        edit = Some(KvEdit::ZAdd(m.into_bytes(), score));
                    }
                    _ => state.error = Some("The score must be a number".into()),
                }
            }
        }
        KvPage::Stream { entries, .. } => {
            table(
                ui,
                p,
                "stream",
                &[("Id", 170.0), ("Fields", 0.0)],
                entries.len(),
                |ui, i| {
                    let (id, fields) = &entries[i];
                    cell(ui, 170.0, id);
                    let line = fields
                        .iter()
                        .map(|(f, v)| format!("{}={}", cell_text(f), cell_text(v)))
                        .collect::<Vec<_>>()
                        .join(" ");
                    cell(ui, value_width(ui), &line);
                    if icons::row_button(ui, icons::DELETE, &format!("Delete {id}")).clicked() {
                        edit = Some(KvEdit::XDel(id.clone()));
                    }
                },
            );
            let new = [("New field", "Field"), ("New value", "Value")];
            if let Some([f, v]) = add_row(ui, state, new, "Add entry") {
                if f.is_empty() {
                    state.error = Some("A stream entry needs a field name".into());
                } else {
                    edit = Some(KvEdit::XAdd(vec![(f.into_bytes(), v.into_bytes())]));
                }
            }
        }
    }
    if !matches!(page, KvPage::String { .. }) {
        let total = match page {
            KvPage::List { total, .. }
            | KvPage::ZSet { total, .. }
            | KvPage::Stream { total, .. } => Some(*total),
            _ => None,
        };
        footer(ui, p, page.len(), total, page.next().is_some(), &mut more);
    }
    if more {
        *action = Some(KvViewAction::LoadMore);
    }
    edit
}

/// The width left for a table's value cell.
fn value_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() - 70.0).max(60.0)
}

/// A table cell of `width`.
fn cell(ui: &mut egui::Ui, width: f32, text: &str) {
    ui.add_sized(
        vec2(width, Spacing::ROW_GRID),
        egui::Label::new(RichText::new(text).monospace()).truncate(),
    );
}

/// Whether an element can be edited inline: text that is not too long.
fn editable(raw: &[u8]) -> Result<(), &'static str> {
    if raw.len() > CELL_EDIT_MAX {
        Err("too large to edit here")
    } else if std::str::from_utf8(raw).is_err() {
        Err("binary: edit it in the console")
    } else {
        Ok(())
    }
}

fn begin_edit(editing: &mut Option<Editing>, row: usize, raw: &[u8]) {
    *editing = Some(Editing {
        row,
        buffer: String::from_utf8_lossy(raw).into_owned(),
        focus: true,
    });
}

/// The edit and remove buttons of a row (`name` makes their accessible names); `editable`
/// says whether the element can be edited and why not.
fn row_buttons(
    ui: &mut egui::Ui,
    name: &str,
    editable: Result<(), &'static str>,
    remove: &str,
) -> (bool, bool) {
    let begin = match editable {
        Ok(()) => icons::row_button(ui, icons::EDIT, &format!("Edit {name}")).clicked(),
        Err(why) => {
            ui.add_enabled_ui(false, |ui| {
                icons::row_button(ui, icons::EDIT, &format!("Edit {name}"))
            })
            .response
            .on_disabled_hover_text(why);
            false
        }
    };
    let remove = icons::row_button(ui, icons::DELETE, &format!("{remove} {name}")).clicked();
    (begin, remove)
}

/// The inline editor of row `row`, when it is the row being edited: the text once saved
/// (with Save, Enter or a `requested` save).
fn edit_field(
    ui: &mut egui::Ui,
    editing: &mut Option<Editing>,
    row: usize,
    label: &str,
    requested: bool,
) -> Option<String> {
    let e = editing.as_mut().filter(|e| e.row == row)?;
    let width = (ui.available_width() - 130.0).max(60.0);
    let response = field(ui, &mut e.buffer, label, "", width);
    if e.focus {
        response.request_focus();
        e.focus = false;
    }
    let enter = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    let escape = response.has_focus() && ui.input(|i| i.key_pressed(Key::Escape));
    let save = ui.button("Save").clicked() || enter || requested;
    let cancel = ui.button("Cancel").clicked() || escape;
    if save {
        let text = e.buffer.clone();
        *editing = None;
        return Some(text);
    }
    if cancel {
        *editing = None;
    }
    None
}

/// A header and the visible rows of a table, `row(ui, index)` drawing each. A column width
/// of 0 takes the rest.
fn table(
    ui: &mut egui::Ui,
    p: &Palette,
    id: &str,
    header: &[(&str, f32)],
    count: usize,
    mut row: impl FnMut(&mut egui::Ui, usize),
) {
    ui.horizontal(|ui| {
        for (label, width) in header {
            let width = if *width == 0.0 {
                value_width(ui)
            } else {
                *width
            };
            ui.add_sized(
                vec2(width, 18.0),
                egui::Label::new(RichText::new(*label).small().color(p.text_muted)),
            );
        }
    });
    let height = (ui.available_height() - 74.0).max(Spacing::ROW_GRID * 3.0);
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, true])
        .max_height(height)
        .show_rows(ui, Spacing::ROW_GRID, count, |ui, range| {
            for i in range {
                ui.horizontal(|ui| {
                    ui.set_min_height(Spacing::ROW_GRID);
                    row(ui, i);
                });
            }
        });
}

/// How much is loaded, and "Load more" while there is more.
fn footer(
    ui: &mut egui::Ui,
    p: &Palette,
    loaded: usize,
    total: Option<u64>,
    has_more: bool,
    more: &mut bool,
) {
    ui.horizontal(|ui| {
        let text = match total {
            Some(total) => format!("{loaded} of {total} loaded"),
            None => format!("{loaded} loaded"),
        };
        ui.label(RichText::new(text).small().color(p.text_muted));
        if has_more && ui.button("Load more").clicked() {
            *more = true;
        }
    });
}

/// A row of labelled fields and a button; the texts once the button is clicked.
fn add_row<const N: usize>(
    ui: &mut egui::Ui,
    state: &mut KvViewState,
    fields: [(&str, &str); N],
    button: &str,
) -> Option<[String; N]> {
    let mut done = None;
    ui.horizontal(|ui| {
        let width = ((ui.available_width() - 90.0) / N as f32 - 8.0).max(60.0);
        for (i, (label, hint)) in fields.iter().enumerate() {
            let text = if i == 0 {
                &mut state.add_a
            } else {
                &mut state.add_b
            };
            field(ui, text, label, hint, width);
        }
        if ui.button(button).clicked() {
            let mut out: [String; N] = std::array::from_fn(|_| String::new());
            for (i, slot) in out.iter_mut().enumerate() {
                *slot = if i == 0 {
                    std::mem::take(&mut state.add_a)
                } else {
                    std::mem::take(&mut state.add_b)
                };
            }
            done = Some(out);
        }
    });
    done
}

/// A read-only text, cut into visual lines and scrolled by rows.
fn text_view(ui: &mut egui::Ui, id: &str, text: &str, rows: &[Range<usize>]) {
    let height = ui.text_style_height(&egui::TextStyle::Monospace);
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show_rows(ui, height, rows.len(), |ui, range| {
            for i in range {
                ui.label(RichText::new(&text[rows[i].clone()]).monospace());
            }
        });
}

fn string_body(
    ui: &mut egui::Ui,
    state: &mut KvViewState,
    bytes: &[u8],
    total_len: u64,
    p: &Palette,
    more: &mut bool,
) -> Option<KvEdit> {
    let valid = std::str::from_utf8(bytes).is_ok();
    let truncated = (bytes.len() as u64) < total_len;
    let print = (bytes.as_ptr() as usize, bytes.len());
    if state.page_print != print {
        state.page_print = print;
        let dirty = state.text != state.original;
        state.original = String::from_utf8_lossy(bytes).into_owned();
        if !dirty {
            state.text = state.original.clone();
        }
        state.rows = None;
        if !state.mode_set {
            state.mode_set = true;
            state.mode = if valid {
                StringMode::Text
            } else {
                StringMode::Hex
            };
        }
    }
    ui.horizontal(|ui| {
        for (mode, label) in [
            (StringMode::Text, "Text"),
            (StringMode::Json, "JSON"),
            (StringMode::Hex, "Hex"),
        ] {
            if theme::segment(ui, state.mode == mode, label).clicked() {
                state.mode = mode;
            }
        }
        let note = if truncated {
            format!(
                "Showing the first {} of {}",
                format_size(bytes.len() as u64),
                format_size(total_len)
            )
        } else {
            format_size(total_len)
        };
        ui.label(RichText::new(note).small().color(p.text_muted));
        if truncated && ui.button("Load more").clicked() {
            *more = true;
        }
    });
    ui.add_space(Spacing::S);
    let can_edit = valid && !truncated && bytes.len() <= EDIT_MAX;
    let mut edit = None;
    if std::mem::take(&mut state.save_requested) && can_edit && state.text != state.original {
        edit = Some(KvEdit::SetString(state.text.clone().into_bytes()));
    }
    match state.mode {
        StringMode::Hex => {
            let shown = &bytes[..bytes.len().min(HEX_MAX)];
            let height = ui.text_style_height(&egui::TextStyle::Monospace);
            egui::ScrollArea::vertical()
                .id_salt("kv_hex")
                .auto_shrink([false, false])
                .show_rows(ui, height, shown.len().div_ceil(16), |ui, range| {
                    for row in range {
                        ui.label(RichText::new(hex_row(shown, row)).monospace());
                    }
                });
        }
        StringMode::Json => {
            let print = (print.0, print.1, StringMode::Json);
            if state.rows.as_ref().is_none_or(|r| r.print != print) {
                let text = pretty_json(&state.original)
                    .unwrap_or_else(|| "Not valid JSON: use the Text view.".to_string());
                let rows = wrap_rows(&text);
                state.rows = Some(TextRows { print, text, rows });
            }
            if let Some(r) = &state.rows {
                text_view(ui, "kv_json", &r.text, &r.rows);
            }
        }
        StringMode::Text => {
            if can_edit {
                let dirty = state.text != state.original;
                ui.horizontal(|ui| {
                    if ui.add_enabled(dirty, egui::Button::new("Save")).clicked() {
                        edit = Some(KvEdit::SetString(state.text.clone().into_bytes()));
                    }
                    if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                        state.text = state.original.clone();
                    }
                });
                egui::ScrollArea::vertical()
                    .id_salt("kv_text")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let response = crate::text_menu::text_edit(
                            ui,
                            TextEdit::multiline(&mut state.text)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .desired_rows(8),
                        );
                        name_edit(&response, "String value");
                    });
            } else {
                let print = (print.0, print.1, StringMode::Text);
                if state.rows.as_ref().is_none_or(|r| r.print != print) {
                    let text = state.original.clone();
                    let rows = wrap_rows(&text);
                    state.rows = Some(TextRows { print, text, rows });
                }
                let why = if truncated {
                    "Read-only: load the whole value to edit it"
                } else if !valid {
                    "Read-only: the value is binary"
                } else {
                    "Read-only: the value is too large to edit here"
                };
                theme::hint(ui, why);
                if let Some(r) = &state.rows {
                    text_view(ui, "kv_text_ro", &r.text, &r.rows);
                }
            }
        }
    }
    edit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttls_parse_with_units() {
        assert_eq!(parse_ttl("90"), Some(90_000));
        assert_eq!(parse_ttl(" 45s "), Some(45_000));
        assert_eq!(parse_ttl("5m"), Some(300_000));
        assert_eq!(parse_ttl("2h"), Some(7_200_000));
        assert_eq!(parse_ttl("1d"), Some(86_400_000));
        assert_eq!(parse_ttl("500ms"), Some(500));
        assert_eq!(parse_ttl("1.5h"), Some(5_400_000));
        for bad in ["", "0", "-5", "5x", "m", "abc", "0.0001s"] {
            assert_eq!(parse_ttl(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn sizes_use_binary_units() {
        assert_eq!(format_size(12), "12 B");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0 MiB");
    }

    #[test]
    fn long_texts_wrap_into_visual_lines() {
        let long = "é".repeat(WRAP_CHARS * 2 + 5);
        let text = format!("one\r\n{long}\n\nlast");
        let rows = wrap_rows(&text);
        let shown: Vec<&str> = rows.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(shown[0], "one");
        assert_eq!(shown[1].chars().count(), WRAP_CHARS);
        assert_eq!(shown[2].chars().count(), WRAP_CHARS);
        assert_eq!(shown[3].chars().count(), 5);
        assert_eq!(shown[4], "");
        assert_eq!(shown[5], "last");
        assert!(wrap_rows("").is_empty());
    }

    #[test]
    fn cells_are_escaped_and_cut() {
        assert_eq!(cell_text(b"a\xffb"), "a\\xffb");
        let long = "x".repeat(CELL_CHARS + 10);
        let shown = cell_text(long.as_bytes());
        assert_eq!(shown.chars().count(), CELL_CHARS + 1);
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn hex_rows_show_offset_bytes_and_text() {
        let bytes: Vec<u8> = (0..20).collect();
        let first = hex_row(&bytes, 0);
        assert!(first.starts_with("00000000  00 01 02"), "{first}");
        assert!(first.ends_with("|................|"), "{first}");
        let second = hex_row(&bytes, 1);
        assert!(second.starts_with("00000010  10 11 12 13"), "{second}");
    }

    #[test]
    fn elements_edit_inline_only_when_small_text() {
        assert!(editable(b"text").is_ok());
        assert!(editable(&[0xff]).is_err());
        assert!(editable(&vec![b'x'; CELL_EDIT_MAX + 1]).is_err());
    }
}
