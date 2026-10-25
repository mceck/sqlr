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

// Listed group by group, in the order of [`GROUPS`]: the Shortcuts dialog and the keymap
// editor show them in this order under one heading per group.
actions! {
    RunStatement => ("run_statement", "Run statement or selection", "Query", Global),
    RunScript => ("run_script", "Run script", "Query", Global),
    Stop => ("stop", "Stop the running query", "Query", Global),
    Explain => ("explain", "Explain", "Query", Global),
    ExplainAnalyze => ("explain_analyze", "Explain analyze (runs the statement)", "Query", Global),
    Commit => ("commit", "Commit the transaction", "Query", Global),
    Rollback => ("rollback", "Roll back the transaction", "Query", Global),
    SaveSnippet => ("save_snippet", "Save as snippet", "Query", Global),
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
    Copy => ("copy", "Copy", "Editor", Global),
    Cut => ("cut", "Cut", "Editor", Global),
    Paste => ("paste", "Paste", "Editor", Global),
    SelectAll => ("select_all", "Select all", "Editor", Editor),
    Newline => ("newline", "Insert newline", "Editor", Editor),
    Indent => ("indent", "Indent", "Editor", Editor),
    Outdent => ("outdent", "Outdent", "Editor", Editor),
    ToggleSoftWrap => ("toggle_soft_wrap", "Toggle soft wrap", "Editor", Global),
    Commands => ("commands", "Command palette", "Navigation", Global),
    OpenObject => ("open_object", "Go to a table or view", "Navigation", Global),
    NewTab => ("new_tab", "New query tab (no connection)", "Navigation", Global),
    NewTabOnConnection => ("new_tab_on_connection", "New query tab on the current connection", "Navigation", Global),
    CloseTab => ("close_tab", "Close tab", "Navigation", Global),
    NextTab => ("next_tab", "Next tab", "Navigation", Global),
    PreviousTab => ("previous_tab", "Previous tab", "Navigation", Global),
    ToggleHistory => ("toggle_history", "Show or hide query history", "View", Global),
    ToggleValuePanel => ("toggle_value_panel", "Show or hide the value panel", "View", Global),
    ToggleTerminal => ("toggle_terminal", "Show or hide the terminal", "View", Global),
    ShowOperations => ("show_operations", "Show operations", "View", Global),
    ShowTasks => ("show_tasks", "Show tasks", "View", Global),
    ExportResult => ("export_result", "Export the result", "Results", Global),
    DeleteRows => ("delete_rows", "Delete selected rows", "Results", Editor),
    RenameSelection => ("rename_selection", "Rename the selected explorer item", "Objects", Explorer),
    ApplyObject => ("apply_object", "Apply the object editor's changes", "Objects", Global),
    NewConnection => ("new_connection", "New connection", "General", Global),
    Settings => ("settings", "Preferences", "General", Global),
    Shortcuts => ("shortcuts", "Keyboard shortcuts", "General", Global),
    Profiles => ("profiles", "Connection profiles", "General", Global),
    DriverManager => ("driver_manager", "Driver Manager", "General", Global),
    TransferData => ("transfer_data", "Import or export data", "General", Global),
    CompareSchemas => ("compare_schemas", "Compare schemas", "General", Global),
    About => ("about", "About", "General", Global),
    Quit => ("quit", "Quit", "General", Global),
}

/// The action groups, in the order they are listed.
pub const GROUPS: &[&str] = &[
    "Query",
    "Editor",
    "Navigation",
    "View",
    "Results",
    "Objects",
    "General",
];

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

    /// How the chord reads on this platform, the one notation every shortcut hint uses.
    /// macOS: modifier glyphs in Apple's order (⌃⌥⇧⌘) then the key, `⇧⌘↵`. Elsewhere: names
    /// joined by `+`, `Ctrl+Shift+Enter`. The platform is a parameter so tests can check both.
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
            out.push_str(key_label(&c.key, true));
            return out;
        }
        let mut parts = Vec::new();
        for (on, name) in [
            (c.mods.cmd, "Ctrl"),
            (c.mods.alt, "Alt"),
            (c.mods.shift, "Shift"),
        ] {
            if on {
                parts.push(name);
            }
        }
        parts.push(key_label(&c.key, false));
        parts.join("+")
    }
}

/// How a key reads in a shortcut hint. Glyphs are kept to the ones the UI font draws (Esc
/// stays a word: ⎋ is unfamiliar and missing from the font).
pub fn key_label(key: &str, mac: bool) -> &str {
    let symbol = match key {
        "Comma" => ",",
        "Period" => ".",
        "Slash" => "/",
        "Backslash" => "\\",
        "Minus" => "-",
        "Equals" => "=",
        "Semicolon" => ";",
        "Colon" => ":",
        "OpenBracket" => "[",
        "CloseBracket" => "]",
        "Backtick" => "`",
        "Quote" => "'",
        "Space" => "Space",
        "Escape" => "Esc",
        _ => "",
    };
    if !symbol.is_empty() {
        return symbol;
    }
    if mac {
        match key {
            "Enter" => "↵",
            "Tab" => "⇥",
            "Backspace" => "⌫",
            "Delete" => "⌦",
            "ArrowUp" => "↑",
            "ArrowDown" => "↓",
            "ArrowLeft" => "←",
            "ArrowRight" => "→",
            "PageUp" => "PgUp",
            "PageDown" => "PgDn",
            other => other,
        }
    } else {
        match key {
            "ArrowUp" => "Up",
            "ArrowDown" => "Down",
            "ArrowLeft" => "Left",
            "ArrowRight" => "Right",
            "Delete" => "Del",
            "PageUp" => "PgUp",
            "PageDown" => "PgDn",
            other => other,
        }
    }
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
    // Explain on ⌥⌘E (E for explain, ⌥ keeps ⌘E free); Analyze adds ⇧ because it runs the
    // statement. Commit and rollback sit on ⌥⌘ next to Run: ⌥⌘↵ "submits" the transaction,
    // ⌥⌘Z undoes it. Ctrl+Alt+Backspace is avoided for rollback: X11 can reserve it to
    // end the session.
    (A::Explain, &["Cmd+Alt+E"]),
    (A::ExplainAnalyze, &["Cmd+Alt+Shift+E"]),
    (A::Commit, &["Cmd+Alt+Enter"]),
    (A::Rollback, &["Cmd+Alt+Z"]),
    (A::NewTab, &["Cmd+T"]),
    (A::NewTabOnConnection, &["Cmd+N"]),
    (A::CloseTab, &["Cmd+W"]),
    (A::NextTab, &["Ctrl+Tab"]),
    (A::PreviousTab, &["Ctrl+Shift+Tab"]),
    (A::ApplyObject, &["Cmd+S"]),
    (A::SaveSnippet, &["Cmd+Shift+S"]),
    (A::OpenObject, &["Cmd+P"]),
    (A::Commands, &["Cmd+Shift+P"]),
    (A::Settings, &["Cmd+Comma"]),
    (A::ToggleTerminal, &["Ctrl+Backtick"]),
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
    (A::DeleteRows, &["Cmd+Backspace", "Cmd+Delete"]),
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

    /// The hint for `action`: its first chord in platform notation ([`Chord::label`]), `None`
    /// while it is unbound.
    pub fn label(&self, action: ActionId, mac: bool) -> Option<String> {
        self.chords_for(action).first().map(|c| c.label(mac))
    }

    /// Every chord of `action` in platform notation, in binding order.
    pub fn labels(&self, action: ActionId, mac: bool) -> Vec<String> {
        self.chords_for(action)
            .iter()
            .map(|c| c.label(mac))
            .collect()
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
        keymap.bind(ActionId::Commit, Chord::parse("Ctrl+Shift+P").unwrap());
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
            assert!(!action.label().is_empty() && GROUPS.contains(&action.group()));
        }
        // Groups are contiguous and listed in GROUPS order, so a list shows one heading each.
        let mut seen: Vec<&str> = Vec::new();
        for action in ActionId::ALL {
            if seen.last() != Some(&action.group()) {
                assert!(!seen.contains(&action.group()), "{} split", action.group());
                seen.push(action.group());
            }
        }
        assert_eq!(seen, GROUPS);
    }

    fn label(chord: &str, mac: bool) -> String {
        Chord::parse(chord).unwrap().label(mac)
    }

    #[test]
    fn hints_use_glyphs_on_macos_and_names_elsewhere() {
        // Each modifier, alone and in Apple's order ⌃⌥⇧⌘.
        assert_eq!(label("Cmd+K", true), "⌘K");
        assert_eq!(label("Shift+K", true), "⇧K");
        assert_eq!(label("Alt+K", true), "⌥K");
        assert_eq!(label("Ctrl+K", true), "⌃K");
        assert_eq!(label("Cmd+Ctrl+Alt+Shift+K", true), "⌃⌥⇧⌘K");
        assert_eq!(label("Cmd+K", false), "Ctrl+K");
        assert_eq!(label("Shift+K", false), "Shift+K");
        assert_eq!(label("Alt+K", false), "Alt+K");
        assert_eq!(label("Cmd+Alt+Shift+K", false), "Ctrl+Alt+Shift+K");
        // Off macOS Control is ⌘'s key: one "Ctrl", never two.
        assert_eq!(label("Ctrl+Backtick", false), "Ctrl+`");
        assert_eq!(label("Cmd+Ctrl+K", false), "Ctrl+K");
        // Special keys.
        for (key, mac, other) in [
            ("Enter", "↵", "Enter"),
            ("Backspace", "⌫", "Backspace"),
            ("Delete", "⌦", "Del"),
            ("Tab", "⇥", "Tab"),
            ("Escape", "Esc", "Esc"),
            ("Space", "Space", "Space"),
            ("Backtick", "`", "`"),
            ("Comma", ",", ","),
            ("Slash", "/", "/"),
            ("Minus", "-", "-"),
            ("ArrowDown", "↓", "Down"),
            ("F2", "F2", "F2"),
        ] {
            assert_eq!(label(key, true), mac, "{key} on macOS");
            assert_eq!(label(key, false), other, "{key} elsewhere");
        }
        assert_eq!(label("Cmd+Alt+Enter", true), "⌥⌘↵");
        assert_eq!(label("Cmd+Alt+Enter", false), "Ctrl+Alt+Enter");
        // Shifted symbols read as their base key with ⇧.
        assert_eq!(label("Cmd+Questionmark", true), "⇧⌘/");
    }

    #[test]
    fn hints_follow_the_keymap() {
        let mut keymap = Keymap::preset("sqlr").unwrap();
        assert_eq!(
            keymap.label(ActionId::Explain, true).as_deref(),
            Some("⌥⌘E")
        );
        assert_eq!(keymap.labels(ActionId::Redo, true), ["⇧⌘Z", "⌘Y"]);
        keymap.set_chords(ActionId::Explain, vec![Chord::parse("F7").unwrap()]);
        assert_eq!(
            keymap.label(ActionId::Explain, false).as_deref(),
            Some("F7")
        );
        keymap.unbind(ActionId::Explain);
        assert_eq!(keymap.label(ActionId::Explain, true), None);
        // The VS Code preset's own chords show through.
        let vscode = Keymap::preset("vscode").unwrap();
        assert_eq!(
            vscode.label(ActionId::FormatSql, true).as_deref(),
            Some("⌥⇧F")
        );
    }

    #[test]
    fn core_actions_are_bound_by_default() {
        let keymap = Keymap::preset("sqlr").unwrap();
        for action in [
            ActionId::Commit,
            ActionId::Rollback,
            ActionId::Explain,
            ActionId::ExplainAnalyze,
        ] {
            assert!(keymap.chord_for(action).is_some(), "{action:?}");
        }
    }
}
