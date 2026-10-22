//! Read-only detail view of one cell: pretty JSON and XML, a hex dump for bytes, and a
//! preview when the bytes are a PNG, JPEG, GIF or WebP image.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::{Id, RichText, TextEdit};
use sqlr_core::{Cell, ColumnMeta, DisplayHint, QueryLanguage, SqlSyntax};

use crate::editor::{Caret, EditorState, Marker, NoCatalog, Pos, query_editor};
use crate::theme;

/// Bytes shown in the hex dump; the rest is counted.
pub(crate) const HEX_DUMP_MAX: usize = 64 * 1024;
/// Larger text is shown as is, without pretty-printing.
const PRETTY_MAX: usize = 4 * 1024 * 1024;
/// Larger JSON is checked on Save only: parsing it after every keystroke would stall the
/// frame.
const LIVE_JSON_MAX: usize = 1024 * 1024;

/// Shows the column name, its type and the full value of the selected cell.
pub fn value_panel(ui: &mut egui::Ui, value: Option<(&ColumnMeta, Cell<'_>)>) {
    let palette = theme::palette(ui.ctx());
    let Some((meta, cell)) = value else {
        ui.label(RichText::new("No cell selected").color(palette.text_muted));
        return;
    };
    ui.label(RichText::new(&meta.name).font(theme::semibold(ui.ctx(), 13.0)));
    ui.label(RichText::new(&meta.type_name).color(palette.text_muted));
    ui.separator();

    let key = cell_key(meta, cell);
    let text = cached_text(ui.ctx(), key, || panel_text(meta, cell));
    let image = match cell {
        Cell::Bytes(bytes) => image_format(bytes).map(|ext| image_uri(ui.ctx(), key, ext, bytes)),
        _ => None,
    };
    egui::ScrollArea::vertical()
        .id_salt("value_panel_scroll")
        .auto_shrink(false)
        .show(ui, |ui| {
            if let Some(uri) = image {
                ui.add(
                    egui::Image::new(uri)
                        .max_width(ui.available_width())
                        .max_height(320.0)
                        .fit_to_original_size(1.0),
                );
                ui.separator();
            }
            // `&str` as `TextBuffer` is read-only but still selectable.
            let mut shown: &str = &text;
            crate::text_menu::text_edit_read_only(
                ui,
                TextEdit::multiline(&mut shown)
                    .font(theme::monospace(ui.ctx()))
                    .desired_width(f32::INFINITY),
            );
        });
}

/// How the value editor shows a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueEditMode {
    Text,
    /// Pretty-printed, highlighted and checked as you type.
    Json,
    /// `0x` and hex digits, for bytes.
    Hex,
}

/// A cell being edited in the value panel (values too long or multi-line for the grid's
/// inline editor, JSON and bytes).
pub struct ValueEdit {
    /// The grid's source row and column of the cell.
    pub row: usize,
    pub col: usize,
    pub mode: ValueEditMode,
    pub text: String,
    pub error: Option<String>,
    /// The query editor of a JSON value: same highlighting, editing and error markers as a
    /// query tab. Created on the first frame.
    editor: Option<Box<EditorState>>,
}

impl Clone for ValueEdit {
    /// Clones the value as text; the in-flight editor (carets, undo) starts fresh.
    fn clone(&self) -> Self {
        Self {
            row: self.row,
            col: self.col,
            mode: self.mode,
            text: self.text.clone(),
            error: self.error.clone(),
            editor: None,
        }
    }
}

impl PartialEq for ValueEdit {
    fn eq(&self, other: &Self) -> bool {
        self.row == other.row
            && self.col == other.col
            && self.mode == other.mode
            && self.text == other.text
            && self.error == other.error
    }
}

impl std::fmt::Debug for ValueEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValueEdit")
            .field("row", &self.row)
            .field("col", &self.col)
            .field("mode", &self.mode)
            .field("text", &self.text)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ValueEditAction {
    Save(sqlr_core::Value),
    SetNull,
    Cancel,
}

impl ValueEdit {
    pub fn new(row: usize, col: usize, meta: &ColumnMeta, value: &sqlr_core::Value) -> Self {
        use sqlr_core::Value;
        let (mode, text) = match value {
            Value::Bytes(b) => (ValueEditMode::Hex, sqlr_core::hex_bytes(b)),
            _ if meta.kind == sqlr_core::ColumnKind::Bytes => (ValueEditMode::Hex, "0x".into()),
            Value::Text(t) if meta.hint == DisplayHint::Json => (
                ValueEditMode::Json,
                sqlr_core::pretty_json(t).unwrap_or_else(|| t.clone()),
            ),
            Value::Null if meta.hint == DisplayHint::Json => (ValueEditMode::Json, String::new()),
            Value::Null => (ValueEditMode::Text, String::new()),
            other => (ValueEditMode::Text, other.display().into_owned()),
        };
        Self {
            row,
            col,
            mode,
            text,
            error: None,
            editor: None,
        }
    }

    /// Replaces the text, dropping the JSON editor's undo history with it.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.error = None;
        self.editor = None;
    }

    /// The edited value, or why it is invalid.
    pub fn save(&self, meta: &ColumnMeta) -> Result<sqlr_core::Value, String> {
        match self.mode {
            ValueEditMode::Hex => crate::grid::parse_hex(&self.text)
                .map(sqlr_core::Value::Bytes)
                .ok_or_else(|| "Not hex bytes: 0x followed by pairs of hex digits".into()),
            ValueEditMode::Json => match sqlr_core::json_error(&self.text) {
                None => Ok(sqlr_core::Value::Text(self.text.clone())),
                Some(err) => Err(format!("Invalid JSON: {}", err.message)),
            },
            ValueEditMode::Text => crate::grid::parse_input(meta, &self.text),
        }
    }
}

/// The value panel in edit mode: the column, a multi-line editor and Save, Set NULL
/// (disabled on a `NOT NULL` column) and Cancel.
pub fn value_editor(
    ui: &mut egui::Ui,
    meta: &ColumnMeta,
    edit: &mut ValueEdit,
    not_null: bool,
) -> Option<ValueEditAction> {
    let palette = theme::palette(ui.ctx());
    let mut action = None;
    ui.label(RichText::new(&meta.name).font(theme::semibold(ui.ctx(), 13.0)));
    let mode = match edit.mode {
        ValueEditMode::Text => "text",
        ValueEditMode::Json => "JSON",
        ValueEditMode::Hex => "hex bytes",
    };
    let mut beautify = false;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{} · editing {mode}", meta.type_name)).color(palette.text_muted),
        );
        if edit.mode == ValueEditMode::Json {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Beautify")
                    .on_hover_text("Reformat the JSON with two-space indentation")
                    .clicked()
                {
                    beautify = true;
                }
            });
        }
    });
    ui.horizontal(|ui| {
        if ui.button("Save").clicked() {
            match edit.save(meta) {
                Ok(value) => action = Some(ValueEditAction::Save(value)),
                Err(e) => edit.error = Some(e),
            }
        }
        let null = ui
            .add_enabled(!not_null, egui::Button::new("Set NULL"))
            .on_disabled_hover_text("The column is NOT NULL");
        if null.clicked() {
            action = Some(ValueEditAction::SetNull);
        }
        if ui.button("Cancel").clicked() {
            action = Some(ValueEditAction::Cancel);
        }
    });
    if beautify {
        beautify_json(edit);
    }
    if let Some(error) = &edit.error {
        ui.colored_label(palette.error, error);
    }
    ui.separator();
    if edit.mode == ValueEditMode::Json {
        json_editor(ui, edit);
        return action;
    }
    egui::ScrollArea::vertical()
        .id_salt("value_editor_scroll")
        .auto_shrink(false)
        .show(ui, |ui| {
            let changed = crate::text_menu::text_edit(
                ui,
                TextEdit::multiline(&mut edit.text)
                    .font(theme::monospace(ui.ctx()))
                    .desired_width(f32::INFINITY),
            )
            .changed();
            if changed {
                edit.error = None;
            }
        });
    action
}

/// Replaces the edited JSON with its pretty-printed form, as one undoable edit.
fn beautify_json(edit: &mut ValueEdit) {
    let Some(pretty) = sqlr_core::pretty_json(&edit.text) else {
        edit.error = Some("Invalid JSON: cannot beautify it".into());
        return;
    };
    if let Some(editor) = &mut edit.editor {
        let end = editor
            .model()
            .buffer()
            .pos_of_char(editor.model().buffer().len_chars());
        editor.set_carets(vec![Caret {
            anchor: Pos::default(),
            head: end,
        }]);
        editor.insert_text(&pretty);
        editor.set_carets(vec![Caret::at(Pos::default())]);
        editor.set_markers(json_markers(&pretty));
        editor.request_focus();
    }
    edit.text = pretty;
    edit.error = None;
}

/// The JSON value editor: the query editor in the JSON language, with the parser's first
/// error underlined (hover for the message) after every edit.
fn json_editor(ui: &mut egui::Ui, edit: &mut ValueEdit) {
    let text = edit.text.clone();
    let editor = edit.editor.get_or_insert_with(|| {
        let mut state = EditorState::new(&text, SqlSyntax::ansi());
        state.set_language(QueryLanguage::Json);
        // A JSON value has no names to complete.
        state.set_completion(false);
        state.set_markers(json_markers(&text));
        Box::new(state)
    });
    let out = query_editor(ui, editor, &NoCatalog);
    if out.changed {
        edit.text = editor.text();
        edit.error = None;
        // Parse once per edit, not once per frame. The marker is painted on the next
        // frame, so ask for it.
        editor.set_markers(json_markers(&edit.text));
        ui.ctx().request_repaint();
    }
}

/// The JSON parser's first error as an editor marker, if any. An empty value is what a NULL
/// starts as, so it is not marked red until the user types something; Save still refuses it.
fn json_markers(text: &str) -> Vec<Marker> {
    if text.trim().is_empty() || text.len() > LIVE_JSON_MAX {
        return Vec::new();
    }
    sqlr_core::json_error(text)
        .map(|err| {
            vec![Marker {
                at: err.at,
                message: err.message,
            }]
        })
        .unwrap_or_default()
}

/// Identifies a cell by its content: a re-run result can reuse the same addresses for
/// other values, so addresses cannot be the key.
fn cell_key(meta: &ColumnMeta, cell: Cell<'_>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    meta.name.hash(&mut h);
    (meta.hint as u8).hash(&mut h);
    match cell {
        Cell::Text(t) => (0u8, t).hash(&mut h),
        Cell::Bytes(b) => (1u8, b).hash(&mut h),
        other => (2u8, other.to_string()).hash(&mut h),
    }
    h.finish()
}

/// The panel text for `key`, computed once while the same cell stays selected.
fn cached_text(ctx: &egui::Context, key: u64, make: impl FnOnce() -> String) -> Arc<String> {
    let id = Id::new("value_panel_text");
    if let Some((k, text)) = ctx.data(|d| d.get_temp::<(u64, Arc<String>)>(id))
        && k == key
    {
        return text;
    }
    let text = Arc::new(make());
    ctx.data_mut(|d| d.insert_temp(id, (key, text.clone())));
    text
}

/// Registers the image bytes once under a URI for the selected cell, forgetting the
/// previous cell's image.
fn image_uri(ctx: &egui::Context, key: u64, ext: &str, bytes: &[u8]) -> String {
    let uri = format!("bytes://sqlr-value-{key:016x}.{ext}");
    let id = Id::new("value_panel_image");
    let previous = ctx.data(|d| d.get_temp::<String>(id));
    if previous.as_deref() != Some(uri.as_str()) {
        if let Some(old) = previous {
            ctx.forget_image(&old);
        }
        ctx.include_bytes(uri.clone(), bytes.to_vec());
        ctx.data_mut(|d| d.insert_temp(id, uri.clone()));
    }
    uri
}

/// The image format the bytes start with, when egui can show it.
pub(crate) fn image_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

fn panel_text(meta: &ColumnMeta, cell: Cell<'_>) -> String {
    match (meta.hint, cell) {
        (_, Cell::Bytes(bytes)) => hex_dump(bytes),
        (DisplayHint::Json, Cell::Text(raw)) if raw.len() <= PRETTY_MAX => {
            sqlr_core::pretty_json(raw).unwrap_or_else(|| raw.to_string())
        }
        (DisplayHint::Xml, Cell::Text(raw)) if raw.len() <= PRETTY_MAX => {
            pretty_xml(raw).unwrap_or_else(|| raw.to_string())
        }
        _ => cell.to_string(),
    }
}

/// `N bytes`, then lines of `offset  16 hex bytes  |ASCII|`.
fn hex_dump(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let unit = if bytes.len() == 1 { "byte" } else { "bytes" };
    let mut out = format!("{} {unit}", bytes.len());
    let shown = &bytes[..bytes.len().min(HEX_DUMP_MAX)];
    for (n, line) in shown.chunks(16).enumerate() {
        let _ = write!(out, "\n{:08x}  ", n * 16);
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
    }
    if bytes.len() > shown.len() {
        let _ = write!(out, "\n… {} more bytes", bytes.len() - shown.len());
    }
    out
}

/// One element, comment or text run per line, indented by nesting; `None` when `raw` does
/// not look like well-nested XML.
fn pretty_xml(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if !raw.starts_with('<') {
        return None;
    }
    let mut out = String::new();
    let mut depth: usize = 0;
    let mut rest = raw;
    let line = |depth: usize, text: &str, out: &mut String| {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&"  ".repeat(depth));
        out.push_str(text);
    };
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix('<') {
            let close = if tail.starts_with("!--") {
                "-->"
            } else if tail.starts_with("![CDATA[") {
                "]]>"
            } else {
                ">"
            };
            let end = rest.find(close)? + close.len();
            let tag = &rest[..end];
            if tag.starts_with("</") {
                depth = depth.checked_sub(1)?;
                line(depth, tag, &mut out);
            } else if tag.starts_with("<?") || tag.starts_with("<!") || tag.ends_with("/>") {
                line(depth, tag, &mut out);
            } else {
                line(depth, tag, &mut out);
                depth += 1;
            }
            rest = &rest[end..];
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let text = rest[..end].trim();
            if !text.is_empty() {
                line(depth, text, &mut out);
            }
            rest = &rest[end..];
        }
    }
    (depth == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::ColumnKind;

    fn meta(hint: DisplayHint) -> ColumnMeta {
        ColumnMeta::new("c", "jsonb", ColumnKind::Text, hint)
    }

    #[test]
    fn bytes_show_a_hex_dump_with_offsets() {
        let m = ColumnMeta::new("b", "bytea", ColumnKind::Bytes, DisplayHint::Binary);
        let bytes: Vec<u8> = (0x41..0x41 + 18).collect();
        assert_eq!(
            panel_text(&m, Cell::Bytes(&bytes)),
            "18 bytes\n\
             00000000  41 42 43 44 45 46 47 48  49 4a 4b 4c 4d 4e 4f 50  |ABCDEFGHIJKLMNOP|\n\
             00000010  51 52                                             |QR|"
        );
        assert_eq!(
            panel_text(&m, Cell::Bytes(&[0, 0xff]))
                .lines()
                .nth(1)
                .unwrap(),
            "00000000  00 ff                                             |..|"
        );
        let big = vec![0u8; HEX_DUMP_MAX + 100];
        let text = panel_text(&m, Cell::Bytes(&big));
        assert!(
            text.ends_with("… 100 more bytes"),
            "{}",
            &text[text.len() - 40..]
        );
        assert_eq!(text.lines().count(), 1 + HEX_DUMP_MAX / 16 + 1);
    }

    #[test]
    fn xml_is_indented() {
        let m = ColumnMeta::new("x", "xml", ColumnKind::Text, DisplayHint::Xml);
        assert_eq!(
            panel_text(
                &m,
                Cell::Text("<?xml version=\"1.0\"?><a x=\"1\"><b>text</b><c/><!-- n --></a>")
            ),
            "<?xml version=\"1.0\"?>\n<a x=\"1\">\n  <b>\n    text\n  </b>\n  <c/>\n  <!-- n -->\n</a>"
        );
        assert_eq!(panel_text(&m, Cell::Text("not xml")), "not xml");
    }

    #[test]
    fn image_formats_by_magic_bytes() {
        assert_eq!(image_format(b"\x89PNG\r\n\x1a\nrest"), Some("png"));
        assert_eq!(image_format(&[0xff, 0xd8, 0xff, 0xe0]), Some("jpeg"));
        assert_eq!(image_format(b"GIF89a.."), Some("gif"));
        assert_eq!(image_format(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(image_format(b"RIFF\0\0\0\0WAVE"), None);
        assert_eq!(image_format(&[1, 2]), None);
    }

    #[test]
    fn json_numbers_are_shown_as_written() {
        let m = meta(DisplayHint::Json);
        assert_eq!(
            panel_text(
                &m,
                Cell::Text("{\"n\": 123456789012345678901234, \"d\": 1.10}")
            ),
            "{\n  \"n\": 123456789012345678901234,\n  \"d\": 1.10\n}"
        );
    }

    #[test]
    fn cache_key_follows_content_not_address() {
        let m = meta(DisplayHint::Plain);
        let mut buf = String::from("aaa");
        let before = cell_key(&m, Cell::Text(&buf));
        buf.replace_range(.., "bbb");
        assert_ne!(before, cell_key(&m, Cell::Text(&buf)));
        let mut bytes = vec![1u8, 2];
        let before = cell_key(&m, Cell::Bytes(&bytes));
        bytes[1] = 3;
        assert_ne!(before, cell_key(&m, Cell::Bytes(&bytes)));
    }

    #[test]
    fn json_is_pretty_printed_only_when_valid() {
        let m = meta(DisplayHint::Json);
        assert_eq!(panel_text(&m, Cell::Text(r#"{"a":1}"#)), "{\n  \"a\": 1\n}");
        assert_eq!(panel_text(&m, Cell::Text("{oops")), "{oops");
        assert_eq!(
            panel_text(&meta(DisplayHint::Plain), Cell::Text(r#"{"a":1}"#)),
            r#"{"a":1}"#
        );
        assert_eq!(panel_text(&m, Cell::Null), "NULL");
    }
}
