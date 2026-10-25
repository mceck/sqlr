//! The classic context menu shared by every text field: undo, redo, cut, copy, paste and
//! select all. Actions are injected as the same [`egui::Event`]s the keyboard produces, so
//! standard text edits and the custom query/JSON editor handle them with their own logic.

use std::sync::Arc;

use egui::{Context, Event, Id, Key, Modifiers, Response, TextEdit, Ui};

use crate::theme::{self, icons};

const PENDING: &str = "sqlr-text-menu-events";
const PLUGIN: &str = "sqlr-text-menu-plugin";

/// What a text field allows; it decides which menu entries are enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextMenuMode {
    /// A normal, writable field.
    Editable,
    /// A selectable but read-only field (copy and select all only).
    ReadOnly,
    /// A password field: pasting and selecting are fine, copying the secret is not.
    Secret,
}

/// Adds a text field with the classic text context menu to `ui`.
pub fn text_edit(ui: &mut Ui, edit: TextEdit<'_>) -> Response {
    let response = ui.add(edit);
    text_edit_menu(&response, TextMenuMode::Editable);
    response
}

/// [`text_edit`] for a selectable but read-only field.
pub fn text_edit_read_only(ui: &mut Ui, edit: TextEdit<'_>) -> Response {
    let response = ui.add(edit);
    text_edit_menu(&response, TextMenuMode::ReadOnly);
    response
}

/// [`text_edit`] for a password field; copy and cut stay disabled.
pub fn text_edit_secret(ui: &mut Ui, edit: TextEdit<'_>) -> Response {
    let response = ui.add(edit);
    text_edit_menu(&response, TextMenuMode::Secret);
    response
}

/// Attaches the classic menu to an already drawn text widget, such as the query editor's
/// custom-painted field or an inline grid cell editor.
pub fn text_edit_menu(response: &Response, mode: TextMenuMode) {
    text_edit_menu_with(response, mode, |_| {});
}

/// [`text_edit_menu`], with `extra` entries appended after the classic ones.
pub fn text_edit_menu_with(response: &Response, mode: TextMenuMode, extra: impl FnOnce(&mut Ui)) {
    preserve_selection(response);
    let enabled = response.enabled();
    let writable = enabled && mode != TextMenuMode::ReadOnly;
    let copy = enabled && mode != TextMenuMode::Secret;
    let cut = writable && mode != TextMenuMode::Secret;
    theme::context_menu(response, |ui| {
        if icons::menu_item_enabled(ui, writable, icons::UNDO, "Undo").clicked() {
            command(response, Key::Z, Modifiers::COMMAND);
            ui.close();
        }
        if icons::menu_item_enabled(ui, writable, icons::REDO, "Redo").clicked() {
            command(response, Key::Z, Modifiers::COMMAND | Modifiers::SHIFT);
            ui.close();
        }
        ui.separator();
        if icons::menu_item_enabled(ui, cut, icons::CUT, "Cut").clicked() {
            event(response, Event::Cut);
            ui.close();
        }
        if icons::menu_item_enabled(ui, copy, icons::COPY, "Copy").clicked() {
            event(response, Event::Copy);
            ui.close();
        }
        if icons::menu_item_enabled(ui, writable, icons::PASTE, "Paste").clicked() {
            if let Some(text) = clipboard_text()
                && !text.is_empty()
            {
                event(response, Event::Paste(text));
            }
            ui.close();
        }
        ui.separator();
        if icons::menu_item_enabled(ui, enabled, icons::SELECT_ALL, "Select all").clicked() {
            command(response, Key::A, Modifiers::COMMAND);
            ui.close();
        }
        extra(ui);
    });
}

fn command(response: &Response, key: Key, modifiers: Modifiers) {
    event(
        response,
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        },
    );
}

/// A right-click on a standard text edit moves the cursor and drops the selection, which
/// would make Copy/Cut from the menu empty. Remember the last selection and put it back when
/// the secondary button goes down.
fn preserve_selection(response: &Response) {
    let ctx = &response.ctx;
    let Some(mut state) = egui::text_edit::TextEditState::load(ctx, response.id) else {
        return;
    };
    let key = response.id.with("sqlr_text_menu_selection");
    let current = state.cursor.char_range();
    if let Some(range) = current.filter(|range| !range.is_empty()) {
        ctx.data_mut(|d| d.insert_temp(key, range));
        return;
    }
    let secondary = response.secondary_clicked() || ctx.input(|i| i.pointer.secondary_down());
    if secondary {
        if let Some(range) = ctx.data(|d| d.get_temp::<egui::text::CCursorRange>(key)) {
            state.cursor.set_char_range(Some(range));
            state.store(ctx, response.id);
        }
    } else if !ctx.input(|i| i.pointer.any_down()) {
        ctx.data_mut(|d| d.remove_temp::<egui::text::CCursorRange>(key));
    }
}

fn event(response: &Response, event: Event) {
    response.request_focus();
    let ctx = &response.ctx;
    ensure_flush(ctx);
    ctx.data_mut(|d| {
        let id = Id::new(PENDING);
        let mut events = d.get_temp::<Vec<Event>>(id).unwrap_or_default();
        events.push(event);
        d.insert_temp(id, events);
    });
}

/// The injected event must reach the next pass: queued events are flushed into that pass's
/// input before any widget runs.
fn ensure_flush(ctx: &Context) {
    let key = Id::new(PLUGIN);
    if ctx.data(|d| d.get_temp::<bool>(key)).is_some() {
        return;
    }
    ctx.on_begin_pass(
        PLUGIN,
        Arc::new(|ui: &mut Ui| {
            let events = ui
                .ctx()
                .data_mut(|d| d.remove_temp::<Vec<Event>>(Id::new(PENDING)));
            if let Some(events) = events {
                ui.input_mut(|i| i.events.extend(events));
            }
        }),
    );
    ctx.data_mut(|d| d.insert_temp(key, true));
}

/// The system clipboard; `None` when the platform clipboard is unavailable.
fn clipboard_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}
