//! The application shell around the panels: toolbar, breadcrumb, status bar, toasts, the
//! window-wide shortcuts and the UI scale.

mod breadcrumb;
mod operations;
pub(crate) mod status;
mod toolbar;

pub(crate) use breadcrumb::tab_breadcrumb;

use egui::Key;
use sqlr_ui::ToastKind;

use crate::app::{SqlrApp, tab_mut};

impl SqlrApp {
    /// Shows `text` as an error toast.
    pub(crate) fn notify(&mut self, text: impl Into<String>) {
        self.toasts.push(ToastKind::Error, text);
    }

    pub(crate) fn notify_as(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(kind, text);
    }

    /// Follows the UI scale setting; egui's own zoom keys keep working until it changes.
    pub(crate) fn apply_ui_scale(&mut self, ctx: &egui::Context) {
        let scale = self.settings.ui_scale;
        if self.applied_ui_scale != Some(scale) {
            ctx.set_zoom_factor(scale);
            self.applied_ui_scale = Some(scale);
        }
    }

    /// At the start of a frame: whether an Esc pressed now may stop the active tab's run.
    /// It may when its editor has the keyboard and nothing to dismiss of its own (a
    /// completion popup, the find bar, a selection or extra carets), and no popup, modal or
    /// palette is open to take the key first.
    pub(crate) fn esc_may_stop(&mut self, ctx: &egui::Context) -> bool {
        if !ctx.input(|i| i.key_pressed(Key::Escape))
            || egui::Popup::is_any_open(ctx)
            || ctx.memory(|m| m.top_modal_layer()).is_some()
            || self.palette.state.mode.is_some()
        {
            return false;
        }
        let Some(key) = self.active_key() else {
            return false;
        };
        let engine = &self.engine;
        tab_mut(&mut self.dock, key).is_some_and(|t| {
            t.redis.is_none()
                && t.editor.has_focus()
                && !t.editor.escape_would_clear()
                && t.is_running(engine)
        })
    }

    /// At the end of a frame: stops the active run when Esc [may stop it](Self::esc_may_stop)
    /// and nothing drawn this frame consumed the key.
    pub(crate) fn esc_stop(&mut self, ctx: &egui::Context, armed: bool) {
        let escape = ctx.input(|i| i.key_pressed(Key::Escape));
        let keymap = sqlr_ui::keymap::ui_keymap(ctx);
        if self.prefs.is_none()
            && (!escape || armed)
            && ctx.input_mut(|i| keymap.consume(i, sqlr_core::ActionId::Stop))
        {
            self.stop_active();
        }
    }

    /// A new editor tab on the active tab's connection, else the explorer's selected one.
    pub(crate) fn new_editor_tab(&mut self) {
        let conn = self
            .active_tab()
            .and_then(|t| t.conn.clone())
            .or_else(|| self.tree.selected.as_ref().map(|(c, _)| c.clone()))
            .filter(|c| self.connection(c).is_some());
        if let Some(c) = conn.as_deref()
            && (self.is_key_value(c) || self.is_document(c))
        {
            let name = self.connection(c).map_or(c, |cfg| cfg.name.as_str());
            let text = format!(
                "{name} does not run SQL: open its databases or collections from the explorer"
            );
            self.notify_as(ToastKind::Info, text);
            return;
        }
        self.new_tab(None, conn, "");
    }
}
