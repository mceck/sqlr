//! The macOS menu bar: File, Edit, View, Query and Help in the system menu, next to the
//! application menu.
//!
//! eframe/winit only install the minimal AppKit application menu (About, Services, Hide,
//! Quit), so the app's menus would stay in the toolbar. `muda` replaces that menu; the
//! commands are the same [`AppCommand`]s the in-window menus and ⌘⇧P use, queued from the
//! AppKit callback and drained at the start of the next frame.
//!
//! Only the real binary installs this (`main.rs`): tests and other platforms keep the
//! in-window menus.

use parking_lot::Mutex;
use std::sync::Arc;

use egui::Context;
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSAboutPanelOptionApplicationIcon, NSAboutPanelOptionApplicationName,
    NSAboutPanelOptionApplicationVersion, NSAboutPanelOptionCredits, NSApplication, NSImage,
};
use objc2_foundation::{NSAttributedString, NSData, NSDictionary, NSString};

use crate::palette::AppCommand;

/// The native menu and the state the app needs to keep in sync with it.
pub struct NativeMenu {
    /// Dropping the root menu removes it from `NSApp`.
    _menu: Menu,
    key_items: Vec<(MenuItem, sqlr_core::ActionId)>,
    keymap_seen: std::cell::RefCell<Option<sqlr_core::Keymap>>,
    value_panel: CheckMenuItem,
    history: CheckMenuItem,
    terminal: CheckMenuItem,
    soft_wrap: CheckMenuItem,
    /// Commands sent by the AppKit callback, drained by the app once per frame.
    pending: Arc<Mutex<Vec<AppCommand>>>,
}

impl NativeMenu {
    /// Builds the menu bar and makes it the application's main menu.
    ///
    /// Must run on the main thread, after AppKit has an `NSApplication`.
    pub fn install(
        ctx: &Context,
        value_panel: bool,
        history: bool,
        terminal: bool,
        soft_wrap: bool,
    ) -> muda::Result<Self> {
        // The application menu. Winit's default menu is replaced, so About, Services,
        // Hide/Hide Others/Show All and Quit must be recreated here. About is custom: the
        // stock panel has no icon or version for a cargo binary, so it opens our own
        // standard AppKit panel with the app's icon and credits.
        let about = MenuItem::new("About sqlr", true, None);
        let settings = MenuItem::new("Settings…", true, Some(cmd(Code::Comma)));
        let quit = MenuItem::new("Quit sqlr", true, Some(cmd(Code::KeyQ)));
        let app = Submenu::with_items(
            "sqlr",
            true,
            &[
                &about,
                &PredefinedMenuItem::separator(),
                &settings,
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::services(Some("Services")),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(Some("Hide sqlr")),
                &PredefinedMenuItem::hide_others(Some("Hide Others")),
                &PredefinedMenuItem::show_all(Some("Show All")),
                &PredefinedMenuItem::separator(),
                &quit,
            ],
        )?;

        // File.
        // Items whose accelerator comes from the keymap get none here: `sync_keymap` sets it.
        let item = |label: &str| MenuItem::new(label, true, None);
        let new_tab = MenuItem::new("New query tab", true, Some(cmd(Code::KeyT)));
        let new_tab_on_connection = item("New query tab on the current connection");
        let close_tab = MenuItem::new("Close tab", true, Some(cmd(Code::KeyW)));
        let new_connection = MenuItem::new("New connection", true, None);
        let profiles = MenuItem::new("Connection profiles…", true, None);
        let import_connections = MenuItem::new("Import…", true, None);
        let export_connections = MenuItem::new("Export…", true, None);
        let operations = MenuItem::new("Operations", true, None);
        let file = Submenu::with_items(
            "File",
            true,
            &[
                &new_tab,
                &new_tab_on_connection,
                &close_tab,
                &PredefinedMenuItem::separator(),
                &new_connection,
                &profiles,
                &PredefinedMenuItem::separator(),
                &import_connections,
                &export_connections,
                &PredefinedMenuItem::separator(),
                &operations,
            ],
        )?;

        // Edit. These are custom items: AppKit's predefined Cut/Copy/Paste send selectors
        // to the responder chain, which egui does not implement. The commands push egui
        // events instead, so they work in the editor and in dialog text fields alike.
        let undo = MenuItem::new("Undo", true, Some(cmd(Code::KeyZ)));
        let redo = MenuItem::new("Redo", true, Some(cmd_shift(Code::KeyZ)));
        let cut = MenuItem::new("Cut", true, Some(cmd(Code::KeyX)));
        let copy = MenuItem::new("Copy", true, Some(cmd(Code::KeyC)));
        let paste = MenuItem::new("Paste", true, Some(cmd(Code::KeyV)));
        let delete = MenuItem::new("Delete", true, None);
        let select_all = MenuItem::new("Select All", true, Some(cmd(Code::KeyA)));
        let find = item("Find…");
        let replace = item("Find and replace…");
        let comment = item("Toggle line comment");
        let duplicate = item("Duplicate line or selection");
        let rename = item("Rename…");
        let delete_rows = item("Delete selected rows");
        let edit = Submenu::with_items(
            "Edit",
            true,
            &[
                &undo,
                &redo,
                &PredefinedMenuItem::separator(),
                &cut,
                &copy,
                &paste,
                &delete,
                &PredefinedMenuItem::separator(),
                &select_all,
                &PredefinedMenuItem::separator(),
                &find,
                &replace,
                &comment,
                &duplicate,
                &PredefinedMenuItem::separator(),
                &rename,
                &delete_rows,
            ],
        )?;

        // View.
        let value_panel_item = CheckMenuItem::new("Value panel", true, value_panel, None);
        let history_item = CheckMenuItem::new("History", true, history, None);
        let terminal_item = CheckMenuItem::new("Terminal", true, terminal, None);
        let soft_wrap_item = CheckMenuItem::new("Soft wrap", true, soft_wrap, None);
        let commands = item("Command palette…");
        let open_object = item("Go to a table or view…");
        let view = Submenu::with_items(
            "View",
            true,
            &[
                &commands,
                &open_object,
                &PredefinedMenuItem::separator(),
                &value_panel_item,
                &history_item,
                &terminal_item,
                &soft_wrap_item,
                &PredefinedMenuItem::separator(),
            ],
        )?;

        // Query.
        let run = MenuItem::new("Run statement", true, Some(cmd(Code::Enter)));
        let run_script = MenuItem::new("Run script", true, Some(cmd_shift(Code::Enter)));
        let stop = MenuItem::new("Stop", true, None);
        let explain = item("Explain");
        let explain_analyze = item("Explain analyze");
        let commit = item("Commit");
        let rollback = item("Rollback");
        let format = item("Format SQL");
        let save_snippet = MenuItem::new("Save as snippet…", true, None);
        let query = Submenu::with_items(
            "Query",
            true,
            &[
                &run,
                &run_script,
                &PredefinedMenuItem::separator(),
                &stop,
                &PredefinedMenuItem::separator(),
                &explain,
                &explain_analyze,
                &PredefinedMenuItem::separator(),
                &commit,
                &rollback,
                &PredefinedMenuItem::separator(),
                &format,
                &save_snippet,
            ],
        )?;

        // Tools: the data tools that otherwise hide in the explorer's context menus.
        let transfer = MenuItem::new("Transfer data…", true, None);
        let compare = MenuItem::new("Compare schemas…", true, None);
        let export_result = MenuItem::new("Export result…", true, None);
        let drivers = MenuItem::new("Drivers…", true, None);
        let tools = Submenu::with_items(
            "Tools",
            true,
            &[
                &transfer,
                &compare,
                &export_result,
                &PredefinedMenuItem::separator(),
                &drivers,
            ],
        )?;

        // Help.
        let shortcuts = MenuItem::new("Keyboard Shortcuts", true, None);
        let check_updates = MenuItem::new("Check for Updates…", true, None);
        let diagnostics = MenuItem::new("Diagnostics…", true, None);
        let report = MenuItem::new("Report a Problem…", true, None);
        let help_about = MenuItem::new("About sqlr", true, None);
        let help = Submenu::with_items(
            "Help",
            true,
            &[
                &shortcuts,
                &PredefinedMenuItem::separator(),
                &check_updates,
                &diagnostics,
                &report,
                &PredefinedMenuItem::separator(),
                &help_about,
            ],
        )?;

        let menu = Menu::with_items(&[&app, &file, &edit, &view, &query, &tools, &help])?;

        // The AppKit callback only knows the item id, so the ids are mapped to commands
        // once, here.
        use sqlr_core::ActionId as A;
        let actions: Vec<(MenuId, AppCommand)> = vec![
            (
                new_tab_on_connection.id().clone(),
                AppCommand::NewTabOnConnection,
            ),
            (find.id().clone(), AppCommand::Key(A::Find)),
            (replace.id().clone(), AppCommand::Key(A::Replace)),
            (comment.id().clone(), AppCommand::Key(A::ToggleComment)),
            (duplicate.id().clone(), AppCommand::Key(A::DuplicateLine)),
            (rename.id().clone(), AppCommand::Key(A::RenameSelection)),
            (delete_rows.id().clone(), AppCommand::Key(A::DeleteRows)),
            (commands.id().clone(), AppCommand::Commands),
            (open_object.id().clone(), AppCommand::OpenObject),
            (transfer.id().clone(), AppCommand::TransferData),
            (compare.id().clone(), AppCommand::CompareSchemas),
            (export_result.id().clone(), AppCommand::ExportResult),
            (drivers.id().clone(), AppCommand::DriverManager),
            (explain.id().clone(), AppCommand::Explain),
            (explain_analyze.id().clone(), AppCommand::ExplainAnalyze),
            (commit.id().clone(), AppCommand::Commit),
            (rollback.id().clone(), AppCommand::Rollback),
            (format.id().clone(), AppCommand::Key(A::FormatSql)),
            (about.id().clone(), AppCommand::About),
            (settings.id().clone(), AppCommand::Settings),
            (quit.id().clone(), AppCommand::Quit),
            (new_tab.id().clone(), AppCommand::NewTab),
            (close_tab.id().clone(), AppCommand::CloseTab),
            (new_connection.id().clone(), AppCommand::NewConnection),
            (profiles.id().clone(), AppCommand::Profiles),
            (
                import_connections.id().clone(),
                AppCommand::ImportConnections,
            ),
            (
                export_connections.id().clone(),
                AppCommand::ExportConnections,
            ),
            (operations.id().clone(), AppCommand::ShowOperations),
            (undo.id().clone(), AppCommand::Undo),
            (redo.id().clone(), AppCommand::Redo),
            (cut.id().clone(), AppCommand::Cut),
            (copy.id().clone(), AppCommand::Copy),
            (paste.id().clone(), AppCommand::Paste),
            (delete.id().clone(), AppCommand::Delete),
            (select_all.id().clone(), AppCommand::SelectAll),
            (run.id().clone(), AppCommand::Run),
            (run_script.id().clone(), AppCommand::RunScript),
            (stop.id().clone(), AppCommand::Stop),
            (save_snippet.id().clone(), AppCommand::SaveSnippet),
            (shortcuts.id().clone(), AppCommand::Shortcuts),
            (check_updates.id().clone(), AppCommand::CheckUpdates),
            (diagnostics.id().clone(), AppCommand::Diagnostics),
            (report.id().clone(), AppCommand::ReportProblem),
            (help_about.id().clone(), AppCommand::About),
            (value_panel_item.id().clone(), AppCommand::ToggleValuePanel),
            (history_item.id().clone(), AppCommand::ToggleHistory),
            (terminal_item.id().clone(), AppCommand::ToggleTerminal),
            (soft_wrap_item.id().clone(), AppCommand::ToggleSoftWrap),
        ];

        let pending: Arc<Mutex<Vec<AppCommand>>> = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::clone(&pending);
        let ctx = ctx.clone();
        let key_items = vec![
            (new_tab_on_connection, A::NewTabOnConnection),
            (find, A::Find),
            (replace, A::Replace),
            (comment, A::ToggleComment),
            (duplicate, A::DuplicateLine),
            (rename, A::RenameSelection),
            (delete_rows, A::DeleteRows),
            (commands, A::Commands),
            (open_object, A::OpenObject),
            (explain, A::Explain),
            (explain_analyze, A::ExplainAnalyze),
            (commit, A::Commit),
            (rollback, A::Rollback),
            (format, A::FormatSql),
            // Not Stop: its Esc would be taken from every popup and dialog.
            (save_snippet, A::SaveSnippet),
            (settings, A::Settings),
            (new_tab, A::NewTab),
            (close_tab, A::CloseTab),
            (operations, A::ShowOperations),
            (run, A::RunStatement),
            (run_script, A::RunScript),
            (undo, A::Undo),
            (redo, A::Redo),
            (select_all, A::SelectAll),
            (cut, A::Cut),
            (copy, A::Copy),
            (paste, A::Paste),
        ];
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, command)) = actions.iter().find(|(id, _)| id == event.id()) {
                queue.lock().push(command.clone());
                // The event loop is idle between frames; wake it for the next pass.
                ctx.request_repaint();
            }
        }));

        menu.init_for_nsapp();
        // AppKit names the first menu after the process (`sqlr-app` for a cargo binary).
        // The app is called sqlr everywhere else, so keep the menu bar consistent.
        app.set_text("sqlr");

        // AppKit does not use winit's window icon: set the application icon explicitly so
        // the Dock and the About panel show sqlr's icon.
        if let Some(icon) = load_icon()
            && let Some(mtm) = MainThreadMarker::new()
        {
            let app = NSApplication::sharedApplication(mtm);
            // SAFETY: `icon` is a valid NSImage, and this runs on the main thread.
            unsafe { app.setApplicationIconImage(Some(&icon)) };
        }

        Ok(Self {
            _menu: menu,
            key_items,
            keymap_seen: Default::default(),
            value_panel: value_panel_item,
            history: history_item,
            terminal: terminal_item,
            soft_wrap: soft_wrap_item,
            pending,
        })
    }

    /// The commands the menu produced since the last frame.
    pub fn drain(&self) -> Vec<AppCommand> {
        std::mem::take(&mut *self.pending.lock())
    }

    pub fn sync_keymap(&self, map: &sqlr_core::Keymap) {
        if self.keymap_seen.borrow().as_ref() == Some(map) {
            return;
        }
        for (item, action) in &self.key_items {
            let accelerator = map.chord_for(*action).and_then(|c| {
                let name = if c.key.len() == 1 && c.key.as_bytes()[0].is_ascii_alphabetic() {
                    format!("Key{}", c.key)
                } else {
                    c.key.clone()
                };
                let code = name.parse::<Code>().ok()?;
                let mut mods = Modifiers::empty();
                if c.mods.cmd {
                    mods |= Modifiers::META;
                }
                if c.mods.ctrl {
                    mods |= Modifiers::CONTROL;
                }
                if c.mods.alt {
                    mods |= Modifiers::ALT;
                }
                if c.mods.shift {
                    mods |= Modifiers::SHIFT;
                }
                Some(Accelerator::new(mods, code))
            });
            if let Err(e) = item.set_accelerator(accelerator) {
                tracing::warn!("could not update shortcut: {e}");
            }
        }
        *self.keymap_seen.borrow_mut() = Some(map.clone());
    }

    /// Mirrors the app's panel state in the View menu's check marks.
    pub fn sync(&self, value_panel: bool, history: bool, terminal: bool, soft_wrap: bool) {
        if self.value_panel.is_checked() != value_panel {
            self.value_panel.set_checked(value_panel);
        }
        if self.terminal.is_checked() != terminal {
            self.terminal.set_checked(terminal);
        }
        if self.soft_wrap.is_checked() != soft_wrap {
            self.soft_wrap.set_checked(soft_wrap);
        }
        if self.history.is_checked() != history {
            self.history.set_checked(history);
        }
    }
}

/// ⌘<key> on macOS.
fn cmd(code: Code) -> Accelerator {
    Accelerator::new(Modifiers::META, code)
}

/// ⇧⌘<key> on macOS.
fn cmd_shift(code: Code) -> Accelerator {
    Accelerator::new(Modifiers::META | Modifiers::SHIFT, code)
}

/// Shows the standard AppKit About panel with sqlr's icon, version and credits.
pub fn show_about() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);

    let name = NSString::from_str("sqlr");
    let version = NSString::from_str(env!("CARGO_PKG_VERSION"));
    let credits = NSString::from_str(
        "A lightweight, fast, universal database client.\n\
         Native Rust + egui · MIT License",
    );
    let credits = NSAttributedString::initWithString(NSAttributedString::alloc(), &credits);
    let Some(icon) = load_icon() else {
        return;
    };

    let objects: [Retained<AnyObject>; 4] = [
        name.into_super().into_super(),
        version.into_super().into_super(),
        credits.into_super().into_super(),
        icon.into_super().into_super(),
    ];
    // SAFETY: These are the documented `NSAboutPanelOptionKey`s, and each value has the
    // type AppKit expects for it.
    let keys = unsafe {
        [
            NSAboutPanelOptionApplicationName,
            NSAboutPanelOptionApplicationVersion,
            NSAboutPanelOptionCredits,
            NSAboutPanelOptionApplicationIcon,
        ]
    };
    let object_refs: [&AnyObject; 4] = [&objects[0], &objects[1], &objects[2], &objects[3]];
    let options = NSDictionary::from_slices(&keys, &object_refs);

    // SAFETY: The dictionary is keyed by `NSAboutPanelOptionKey`s with matching values.
    unsafe { app.orderFrontStandardAboutPanelWithOptions(&options) };
}

fn load_icon() -> Option<Retained<NSImage>> {
    let bytes = crate::icon::native_icon_png()?;
    let data = NSData::with_bytes(&bytes);
    NSImage::initWithData(NSImage::alloc(), &data)
}
