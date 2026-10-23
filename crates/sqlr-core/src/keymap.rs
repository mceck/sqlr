//! The keymap (§4.3): every command a shortcut can run, the chords bound to them, presets
//! and conflict detection. Keys are names (`Enter`, `A`, `Comma`), so this stays free of the
//! UI toolkit; the app turns a [`Chord`] into the toolkit's key and modifiers.

use std::collections::BTreeMap;

/// Where a command's chord is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Anywhere in the window.
    Global,
    /// While a text editor has the keyboard.
    Editor,
    Explorer,
}

macro_rules! actions {
    ($( $variant:ident => ($id:literal, $label:literal, $group:literal, $scope:ident) ),+ $(,)?) => {
        /// A command a shortcut can run.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum ActionId { $($variant),+ }

        impl ActionId {
            /// Every action, in the order the keymap editor lists them.
            pub const ALL: &'static [ActionId] = &[$(ActionId::$variant),+];

            /// The stable id used in `keymap.toml`.
            pub fn id(self) -> &'static str {
                match self { $(ActionId::$variant => $id),+ }
            }

            pub fn label(self) -> &'static str {
                match self { $(ActionId::$variant => $label),+ }
            }

            /// The group the keymap editor files the action under.
            pub fn group(self) -> &'static str {
                match self { $(ActionId::$variant => $group),+ }
            }

            pub fn scope(self) -> Scope {
                match self { $(ActionId::$variant => Scope::$scope),+ }
            }

            pub fn from_id(id: &str) -> Option<ActionId> {
                match id { $($id => Some(ActionId::$variant),)+ _ => None }
            }
        }
    };
}

actions! {
    RunStatement => ("run_statement", "Run statement or selection", "Running", Global),
    RunScript => ("run_script", "Run script", "Running", Global),
    Stop => ("stop", "Stop the running query", "Running", Global),
    Commit => ("commit", "Commit", "Running", Global),
    Rollback => ("rollback", "Rollback", "Running", Global),
    Explain => ("explain", "Explain", "Running", Global),
    ExplainAnalyze => ("explain_analyze", "Explain analyze", "Running", Global),
    NewTab => ("new_tab", "New query tab", "Tabs", Global),
    NewTabOnConnection => ("new_tab_on_connection", "New query tab on the current connection", "Tabs", Global),
    CloseTab => ("close_tab", "Close tab", "Tabs", Global),
    NextTab => ("next_tab", "Next tab", "Tabs", Global),
    PreviousTab => ("previous_tab", "Previous tab", "Tabs", Global),
    ApplyObject => ("apply_object", "Apply the object editor's changes", "Tabs", Global),
    OpenObject => ("open_object", "Open an object", "Navigation", Global),
    Commands => ("commands", "Command palette", "Navigation", Global),
    Settings => ("settings", "Preferences", "Navigation", Global),
    ShowOperations => ("show_operations", "Show operations", "Navigation", Global),
    ToggleHistory => ("toggle_history", "Show or hide history", "Navigation", Global),
    ToggleValuePanel => ("toggle_value_panel", "Show or hide the value panel", "Navigation", Global),
    NewConnection => ("new_connection", "New connection", "Navigation", Global),
    TransferData => ("transfer_data", "Import or export data", "Navigation", Global),
    CompareSchemas => ("compare_schemas", "Compare schemas", "Navigation", Global),
    ExportResult => ("export_result", "Export the result", "Navigation", Global),
    SaveSnippet => ("save_snippet", "Save as snippet", "Navigation", Global),
    FormatSql => ("format_sql", "Format SQL", "Editor", Editor),
    ToggleComment => ("toggle_comment", "Toggle line comment", "Editor", Editor),
    DuplicateLine => ("duplicate_line", "Duplicate line or selection", "Editor", Editor),
    Find => ("find", "Find", "Editor", Editor),
    Replace => ("replace", "Find and replace", "Editor", Editor),
    FindNext => ("find_next", "Find next", "Editor", Editor),
    FindPrevious => ("find_previous", "Find previous", "Editor", Editor),
    Complete => ("complete", "Complete", "Editor", Editor),
    Undo => ("undo", "Undo", "Editor", Editor),
    Redo => ("redo", "Redo", "Editor", Editor),
    RenameSelection => ("rename_selection", "Rename selected explorer item", "Navigation", Explorer),
    Newline => ("newline", "Insert newline", "Editor", Editor),
    Indent => ("indent", "Indent", "Editor", Editor),
    Outdent => ("outdent", "Outdent", "Editor", Editor),
    Copy => ("copy", "Copy", "Editor", Global),
    Cut => ("cut", "Cut", "Editor", Global),
    Paste => ("paste", "Paste", "Editor", Global),
    DriverManager => ("driver_manager", "Driver Manager", "Navigation", Global),
    Quit => ("quit", "Quit", "Navigation", Global),
    About => ("about", "About", "Navigation", Global),
    ToggleSoftWrap => ("toggle_soft_wrap", "Toggle soft wrap", "Editor", Global),
    Profiles => ("profiles", "Connection profiles", "Navigation", Global),
    ShowTasks => ("show_tasks", "Show tasks", "Navigation", Global),
    Shortcuts => ("shortcuts", "Keyboard shortcuts", "Navigation", Global),
    SelectAll => ("select_all", "Select all", "Editor", Editor),
}

/// The modifier keys of a chord. `cmd` is ⌘ on macOS and Ctrl elsewhere (egui's `command`);
/// `ctrl` is the Control key itself, which on other platforms is the same key as `cmd`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Mods {
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        cmd: false,
        shift: false,
        alt: false,
        ctrl: false,
    };
    pub const CMD: Mods = Mods {
        cmd: true,
        ..Mods::NONE
    };
}

/// One key press with its modifiers.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    /// The key's name: `A`, `Enter`, `Comma`, `F5`, `ArrowDown`…
    pub key: String,
    pub mods: Mods,
}

impl Chord {
    pub fn new(mods: Mods, key: &str) -> Self {
        Self {
            key: key.to_string(),
            mods,
        }
    }

    /// `Cmd+Shift+Enter`: modifiers in the order Cmd, Ctrl, Alt, Shift, then the key.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.mods.cmd, "Cmd"),
            (self.mods.ctrl, "Ctrl"),
            (self.mods.alt, "Alt"),
            (self.mods.shift, "Shift"),
        ] {
            if on {
                out.push_str(name);
                out.push('+');
            }
        }
        out.push_str(&self.key);
        out
    }

    /// The inverse of [`Chord::to_text`]; `None` for an empty or malformed chord.
    pub fn parse(text: &str) -> Option<Chord> {
        let mut mods = Mods::NONE;
        let mut key = None;
        for part in text.split('+') {
            match part.trim() {
                "Cmd" => mods.cmd = true,
                "Ctrl" => mods.ctrl = true,
                "Alt" => mods.alt = true,
                "Shift" => mods.shift = true,
                "" => return None,
                k if key.is_none() && k.chars().all(|c| c.is_ascii_alphanumeric()) => {
                    key = Some(k.to_string());
                }
                _ => return None,
            }
        }
        Some(Chord { key: key?, mods })
    }

    /// The chord as the platform sees it: off macOS ⌘ and Control are one key.
    pub fn normalized(&self, mac: bool) -> Chord {
        let mut c = self.clone();
        let base = match c.key.as_str() {
            "Questionmark" | "?" => Some("Slash"),
            "Colon" | ":" => Some("Semicolon"),
            "Plus" | "+" => Some("Equals"),
            "Exclamationmark" | "!" => Some("1"),
            _ => None,
        };
        if let Some(base) = base {
            c.key = base.into();
            c.mods.shift = true;
        }
        if !mac && c.mods.ctrl {
            c.mods.cmd = true;
            c.mods.ctrl = false;
        }
        c
    }

    /// How the chord reads on this platform: `⇧⌘↵` on macOS, `Ctrl+Shift+Enter` elsewhere.
    pub fn label(&self, mac: bool) -> String {
        let c = self.normalized(mac);
        if mac {
            let mut out = String::new();
            for (on, glyph) in [
                (c.mods.ctrl, "⌃"),
                (c.mods.alt, "⌥"),
                (c.mods.shift, "⇧"),
                (c.mods.cmd, "⌘"),
            ] {
                if on {
                    out.push_str(glyph);
                }
            }
            out.push_str(&key_glyph(&c.key));
            return out;
        }
        let mut parts = Vec::new();
        for (on, name) in [
            (c.mods.cmd, "Ctrl"),
            (c.mods.alt, "Alt"),
            (c.mods.shift, "Shift"),
        ] {
            if on {
                parts.push(name.to_string());
            }
        }
        parts.push(c.key);
        parts.join("+")
    }
}

fn key_glyph(key: &str) -> String {
    match key {
        "Enter" => "↵",
        "Comma" => ",",
        "Period" => ".",
        "Slash" => "/",
        "Backslash" => "\\",
        "Minus" => "−",
        "Equals" => "=",
        "Semicolon" => ";",
        "Colon" => ":",
        "OpenBracket" => "[",
        "CloseBracket" => "]",
        "Backtick" => "`",
        "Quote" => "'",
        "Space" => "Space",
        "Tab" => "⇥",
        "Escape" => "⎋",
        "Backspace" => "⌫",
        "Delete" => "⌦",
        "ArrowUp" => "↑",
        "ArrowDown" => "↓",
        "ArrowLeft" => "←",
        "ArrowRight" => "→",
        other => return other.to_string(),
    }
    .to_string()
}

/// The preset names the keymap editor offers, with their labels.
pub const PRESETS: &[(&str, &str)] = &[("sqlr", "sqlr"), ("vscode", "VS Code")];

/// The chords of each action.
#[derive(Clone, Debug, Default)]
pub struct Keymap {
    bindings: Vec<(ActionId, Chord)>,
}

/// Two keymaps are equal when they bind the same chords, whatever the order.
impl PartialEq for Keymap {
    fn eq(&self, other: &Self) -> bool {
        let sorted = |k: &Keymap| {
            let mut b = k.bindings.clone();
            b.sort();
            b
        };
        sorted(self) == sorted(other)
    }
}

impl Eq for Keymap {}

type Table = &'static [(ActionId, &'static [&'static str])];

use ActionId as A;

const SQLR: Table = &[
    (A::Stop, &["Escape"]),
    (A::RenameSelection, &["F2"]),
    (A::Newline, &["Enter"]),
    (A::Indent, &["Tab"]),
    (A::Outdent, &["Shift+Tab"]),
    (A::RunStatement, &["Cmd+Enter"]),
    (A::RunScript, &["Cmd+Shift+Enter"]),
    (A::NewTab, &["Cmd+T"]),
    (A::NewTabOnConnection, &["Cmd+N"]),
    (A::CloseTab, &["Cmd+W"]),
    (A::NextTab, &["Ctrl+Tab"]),
    (A::PreviousTab, &["Ctrl+Shift+Tab"]),
    (A::ApplyObject, &["Cmd+S"]),
    (A::OpenObject, &["Cmd+P"]),
    (A::Commands, &["Cmd+Shift+P"]),
    (A::Settings, &["Cmd+Comma"]),
    (A::ShowOperations, &["Cmd+Shift+O"]),
    (A::FormatSql, &["Cmd+Alt+L"]),
    (A::ToggleComment, &["Cmd+Slash"]),
    (A::DuplicateLine, &["Cmd+D"]),
    (A::Find, &["Cmd+F"]),
    (A::Replace, &["Cmd+Alt+F"]),
    (A::FindNext, &["Cmd+G"]),
    (A::FindPrevious, &["Cmd+Shift+G"]),
    (A::Complete, &["Ctrl+Space"]),
    (A::Copy, &["Cmd+C"]),
    (A::Cut, &["Cmd+X"]),
    (A::Paste, &["Cmd+V"]),
    (A::Undo, &["Cmd+Z"]),
    (A::Redo, &["Cmd+Shift+Z", "Cmd+Y"]),
    (A::SelectAll, &["Cmd+A"]),
];

const VSCODE: Table = &[
    (A::FormatSql, &["Alt+Shift+F"]),
    (A::OpenObject, &["Cmd+P"]),
    (A::Commands, &["Cmd+Shift+P"]),
    (A::DuplicateLine, &["Alt+Shift+ArrowDown"]),
    (A::ToggleComment, &["Cmd+Slash"]),
    (A::ShowOperations, &["Cmd+Shift+U"]),
    (A::Complete, &["Ctrl+Space"]),
];

impl Keymap {
    fn from_table(table: Table) -> Keymap {
        let mut keymap = Keymap::default();
        for (action, chords) in table {
            for chord in *chords {
                if let Some(chord) = Chord::parse(chord) {
                    keymap.bindings.push((*action, chord));
                }
            }
        }
        keymap
    }

    /// A named preset, `None` for an unknown name. Presets other than `sqlr` list what they
    /// change; the rest keep the `sqlr` bindings unless a chord is taken.
    pub fn preset(name: &str) -> Option<Keymap> {
        let overrides = match name {
            "sqlr" => return Some(Keymap::from_table(SQLR)),
            "vscode" => VSCODE,
            _ => return None,
        };
        let mut keymap = Keymap::from_table(SQLR);
        let changed = Keymap::from_table(overrides);
        for action in changed.bindings.iter().map(|(a, _)| *a) {
            keymap.unbind(action);
        }
        // An inherited chord that a changed action now uses is dropped.
        for (_, chord) in &changed.bindings {
            keymap.bindings.retain(|(_, c)| c != chord);
        }
        keymap.bindings.extend(changed.bindings);
        Some(keymap)
    }

    pub fn bindings(&self) -> &[(ActionId, Chord)] {
        &self.bindings
    }

    pub fn chords_for(&self, action: ActionId) -> Vec<&Chord> {
        self.bindings
            .iter()
            .filter(|(a, _)| *a == action)
            .map(|(_, c)| c)
            .collect()
    }

    /// The first chord bound to `action`.
    pub fn chord_for(&self, action: ActionId) -> Option<Chord> {
        self.chords_for(action).first().map(|c| (*c).clone())
    }

    /// Replaces every chord of `action` with `chords`.
    pub fn set_chords(&mut self, action: ActionId, chords: Vec<Chord>) {
        self.unbind(action);
        self.bindings
            .extend(chords.into_iter().map(|chord| (action, chord)));
    }

    pub fn unbind(&mut self, action: ActionId) {
        self.bindings.retain(|(a, _)| *a != action);
    }

    /// Adds `chord` to `action` (once).
    pub fn bind(&mut self, action: ActionId, chord: Chord) {
        if !self.bindings.contains(&(action, chord.clone())) {
            self.bindings.push((action, chord));
        }
    }

    /// Chords bound to more than one action whose scopes overlap, as the platform sees them
    /// (`mac`: ⌘ and Control are different keys; elsewhere they are one).
    pub fn conflicts(&self, mac: bool) -> Vec<(Chord, Vec<ActionId>)> {
        let mut by_chord: BTreeMap<Chord, Vec<ActionId>> = BTreeMap::new();
        for (action, chord) in &self.bindings {
            let list = by_chord.entry(chord.normalized(mac)).or_default();
            if !list.contains(action) {
                list.push(*action);
            }
        }
        by_chord
            .into_iter()
            .filter(|(_, actions)| {
                actions.iter().enumerate().any(|(i, a)| {
                    actions[i + 1..].iter().any(|b| {
                        a.scope() == b.scope()
                            || a.scope() == Scope::Global
                            || b.scope() == Scope::Global
                    })
                })
            })
            .collect()
    }

    /// The actions an exact chord is bound to (usually one).
    pub fn actions_for(&self, chord: &Chord, mac: bool) -> Vec<ActionId> {
        let wanted = chord.normalized(mac);
        self.bindings
            .iter()
            .filter(|(_, c)| c.normalized(mac) == wanted)
            .map(|(a, _)| *a)
            .collect()
    }

    /// What differs from `preset`, as `(action id, chords as text)`: what `keymap.toml`
    /// stores besides the preset's name.
    pub fn to_parts(&self, preset: &str) -> Vec<(&'static str, Vec<String>)> {
        let base = Keymap::preset(preset).unwrap_or_else(|| Keymap::from_table(SQLR));
        let text = |k: &Keymap, a: ActionId| -> Vec<String> {
            k.chords_for(a).iter().map(|c| c.to_text()).collect()
        };
        ActionId::ALL
            .iter()
            .filter(|a| text(self, **a) != text(&base, **a))
            .map(|a| (a.id(), text(self, *a)))
            .collect()
    }

    /// The preset (an unknown name means `sqlr`) with the listed actions' chords replaced.
    /// Unknown actions and chords that do not parse are skipped. Returns the keymap and the
    /// preset's name.
    pub fn from_parts(
        preset: &str,
        parts: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> (Keymap, String) {
        let preset = if Keymap::preset(preset).is_some() {
            preset
        } else {
            "sqlr"
        };
        let mut keymap = Keymap::preset(preset).unwrap_or_default();
        for (id, chords) in parts {
            if let Some(action) = ActionId::from_id(&id) {
                let parsed = chords.iter().filter_map(|c| Chord::parse(c)).collect();
                keymap.set_chords(action, parsed);
            }
        }
        (keymap, preset.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_have_no_conflicts() {
        for (name, _) in PRESETS {
            let keymap = Keymap::preset(name).unwrap();
            for mac in [true, false] {
                assert_eq!(keymap.conflicts(mac), vec![], "{name} (mac: {mac})");
            }
            // Every action that a preset binds is a known one with a parseable chord.
            for (_, chord) in keymap.bindings() {
                assert_eq!(Chord::parse(&chord.to_text()).as_ref(), Some(chord));
            }
        }
        assert!(Keymap::preset("emacs").is_none());
    }

    #[test]
    fn keymap_conflicts_and_platform_mapping() {
        let mut keymap = Keymap::preset("sqlr").unwrap();
        // Two global actions on one chord conflict.
        keymap.bind(ActionId::Commit, Chord::parse("Cmd+Shift+P").unwrap());
        let found = keymap.conflicts(true);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, Chord::parse("Cmd+Shift+P").unwrap());
        assert_eq!(found[0].1, [ActionId::Commands, ActionId::Commit]);

        // An editor action and a global one on one chord conflict too: the global one wins
        // before the editor sees the key.
        let mut keymap = Keymap::preset("sqlr").unwrap();
        keymap.bind(ActionId::Find, Chord::parse("Cmd+P").unwrap());
        assert_eq!(keymap.conflicts(true).len(), 1);

        // Off macOS, ⌘ and Control are one key; on macOS they are two.
        let mut keymap = Keymap::preset("sqlr").unwrap();
        keymap.bind(ActionId::Commit, Chord::parse("Ctrl+K").unwrap());
        assert_eq!(keymap.conflicts(true), vec![]);
        assert_eq!(keymap.conflicts(false).len(), 1);

        // Shifted and unshifted chords are different chords.
        let mut keymap = Keymap::preset("sqlr").unwrap();
        keymap.bind(ActionId::Commit, Chord::parse("Cmd+Shift+K").unwrap());
        assert_eq!(keymap.conflicts(true), vec![]);

        // Labels follow the platform.
        let run_script = Chord::parse("Cmd+Shift+Enter").unwrap();
        assert_eq!(run_script.label(true), "⇧⌘↵");
        assert_eq!(run_script.label(false), "Ctrl+Shift+Enter");
        assert_eq!(Chord::parse("Ctrl+Space").unwrap().label(true), "⌃Space");
        assert_eq!(
            Chord::parse("Ctrl+Space").unwrap().label(false),
            "Ctrl+Space"
        );
        assert_eq!(Chord::parse("Cmd+Comma").unwrap().label(true), "⌘,");
        // Malformed chords do not parse.
        for bad in ["", "Cmd+", "+A", "Cmd+A+B", "Cmd+É"] {
            assert_eq!(Chord::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn keymap_parts_round_trip() {
        // The default keymap differs from nothing.
        let sqlr = Keymap::preset("sqlr").unwrap();
        assert_eq!(sqlr.to_parts("sqlr"), vec![]);
        assert_eq!(
            Keymap::from_parts("sqlr", []),
            (sqlr.clone(), "sqlr".into())
        );

        // Changes are a difference from the preset, and read back whole.
        let mut custom = Keymap::preset("vscode").unwrap();
        custom.set_chords(ActionId::RunStatement, vec![Chord::parse("Alt+R").unwrap()]);
        custom.unbind(ActionId::Find);
        let parts = custom.to_parts("vscode");
        assert_eq!(
            parts,
            vec![
                ("run_statement", vec!["Alt+R".to_string()]),
                ("find", vec![]),
            ]
        );
        let owned = parts.into_iter().map(|(id, c)| (id.to_string(), c));
        assert_eq!(
            Keymap::from_parts("vscode", owned),
            (custom, "vscode".into())
        );

        // Unknown presets, actions and chords are ignored.
        let (odd, preset) = Keymap::from_parts(
            "emacs",
            [
                ("teleport".to_string(), vec!["Cmd+T".to_string()]),
                (
                    "find".to_string(),
                    vec!["nonsense++".to_string(), "Cmd+F".to_string()],
                ),
            ],
        );
        assert_eq!(preset, "sqlr");
        assert_eq!(odd, sqlr);
    }

    #[test]
    fn every_action_has_an_id_and_round_trips() {
        let mut ids = std::collections::HashSet::new();
        for action in ActionId::ALL {
            assert!(ids.insert(action.id()), "{}", action.id());
            assert_eq!(ActionId::from_id(action.id()), Some(*action));
            assert!(!action.label().is_empty() && !action.group().is_empty());
        }
    }
}
