//! The keymap as widgets use it: the chords the editor, its find bar and the grid match
//! themselves, set once a frame by the app, and the conversions between a core [`Chord`] and
//! egui's keys and modifiers.

use std::sync::Arc;

use egui::{Context, InputState, Key, KeyboardShortcut, Modifiers};
use sqlr_core::{ActionId, Chord, Keymap, Mods};

/// A key's name in a [`Chord`]; arrows are `ArrowUp` and so on, every other key is egui's
/// own name (`A`, `Enter`, `Comma`, `F5`).
pub fn key_name(key: Key) -> String {
    match key {
        Key::ArrowUp => "ArrowUp".into(),
        Key::ArrowDown => "ArrowDown".into(),
        Key::ArrowLeft => "ArrowLeft".into(),
        Key::ArrowRight => "ArrowRight".into(),
        other => other.name().to_string(),
    }
}

/// The key a chord's name stands for.
pub fn parse_key(name: &str) -> Option<Key> {
    Key::from_name(name)
}

/// The chord a key press with `m` is, as the keymap names it: `cmd` is egui's `command` and
/// `ctrl` only counts on macOS, where it is a key of its own.
pub fn event_chord(key: Key, m: Modifiers, mac: bool) -> Chord {
    Chord {
        key: key_name(key),
        mods: Mods {
            cmd: m.command || (!mac && m.ctrl),
            ctrl: mac && m.ctrl,
            alt: m.alt,
            shift: m.shift,
        },
    }
}

/// The egui modifiers a chord asks for on this platform.
pub fn chord_modifiers(chord: &Chord, mac: bool) -> Modifiers {
    let chord = chord.normalized(mac);
    let mut m = Modifiers::NONE;
    if chord.mods.cmd {
        m |= Modifiers::COMMAND;
    }
    if chord.mods.ctrl {
        m |= Modifiers::CTRL;
    }
    if chord.mods.alt {
        m |= Modifiers::ALT;
    }
    if chord.mods.shift {
        m |= Modifiers::SHIFT;
    }
    m
}

/// The shortcut egui matches for a chord; `None` for a key egui does not know.
pub fn chord_shortcut(chord: &Chord, mac: bool) -> Option<KeyboardShortcut> {
    Some(KeyboardShortcut::new(
        chord_modifiers(chord, mac),
        parse_key(&chord.key)?,
    ))
}

/// The chords of a keymap, for the widgets that match keys themselves.
#[derive(Clone, Debug, PartialEq)]
pub struct UiKeymap {
    keymap: Keymap,
    mac: bool,
}

impl UiKeymap {
    pub fn new(keymap: Keymap, mac: bool) -> Self {
        Self { keymap, mac }
    }

    /// The `sqlr` preset.
    pub fn default_for(mac: bool) -> Self {
        Self::new(Keymap::preset("sqlr").unwrap_or_default(), mac)
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// The editor action a key press is bound to.
    pub fn lookup(&self, key: Key, m: Modifiers) -> Option<ActionId> {
        let chord = event_chord(key, m, self.mac);
        self.keymap
            .actions_for(&chord, self.mac)
            .into_iter()
            .find(|a| {
                a.scope() == sqlr_core::Scope::Editor
                    || matches!(a, ActionId::Copy | ActionId::Cut | ActionId::Paste)
            })
    }

    /// Consumes a press of any chord of `action`; true if there was one. Callers that test
    /// two actions on one key (Find next and Find previous) ask for the more specific first.
    pub fn consume(&self, input: &mut InputState, action: ActionId) -> bool {
        let events: Vec<_> = input
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Key {
                    key,
                    modifiers,
                    pressed: true,
                    ..
                } if self
                    .keymap
                    .actions_for(&event_chord(*key, *modifiers, self.mac), self.mac)
                    .contains(&action) =>
                {
                    Some((*key, *modifiers))
                }
                _ => None,
            })
            .collect();
        events.into_iter().fold(false, |pressed, (key, mods)| {
            input.consume_key(mods, key) || pressed
        })
    }

    /// How `action`'s first chord reads on this platform.
    pub fn label(&self, action: ActionId) -> Option<String> {
        self.keymap.chord_for(action).map(|c| c.label(self.mac))
    }
}

fn data_id() -> egui::Id {
    egui::Id::new("sqlr_ui_keymap")
}

/// The keymap the widgets of this context use: the app's, else the `sqlr` preset.
pub fn ui_keymap(ctx: &Context) -> Arc<UiKeymap> {
    ctx.data(|d| d.get_temp::<Arc<UiKeymap>>(data_id()))
        .unwrap_or_else(|| {
            let mac = ctx.os() == egui::os::OperatingSystem::Mac;
            Arc::new(UiKeymap::default_for(mac))
        })
}

/// Sets the keymap the widgets of this context use.
pub fn set_ui_keymap(ctx: &Context, keymap: Keymap) {
    let mac = ctx.os() == egui::os::OperatingSystem::Mac;
    let next = UiKeymap::new(keymap, mac);
    if ctx
        .data(|d| d.get_temp::<Arc<UiKeymap>>(data_id()))
        .as_deref()
        != Some(&next)
    {
        ctx.data_mut(|d| d.insert_temp(data_id(), Arc::new(next)));
    }
}

/// eframe supplies native clipboard events instead of key events. Restore the key
/// when its platform shortcut was rebound, so the old binding cannot bypass the map.
pub fn normalize_platform_clipboard(ctx: &Context, map: &Keymap) {
    let mac = ctx.os() == egui::os::OperatingSystem::Mac;
    let requested = ctx
        .data(|d| d.get_temp::<bool>(egui::Id::new("sqlr_requested_paste")))
        .unwrap_or(false);
    let mut received = false;
    ctx.input_mut(|input| {
        let mods = input.modifiers;
        for event in &mut input.events {
            let (key, action) = match event {
                egui::Event::Copy => (Key::C, ActionId::Copy),
                egui::Event::Cut => (Key::X, ActionId::Cut),
                egui::Event::Paste(_) if requested => {
                    received = true;
                    continue;
                }
                egui::Event::Paste(_) => (Key::V, ActionId::Paste),
                _ => continue,
            };
            if !mods.command
                || map
                    .actions_for(&event_chord(key, mods, mac), mac)
                    .contains(&action)
            {
                continue;
            }
            *event = egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: mods,
            };
        }
    });
    if received {
        ctx.data_mut(|d| d.remove::<bool>(egui::Id::new("sqlr_requested_paste")));
    }
}

pub fn request_paste(ctx: &Context) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("sqlr_requested_paste"), true));
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_name_round_trips() {
        for key in Key::ALL {
            let name = key_name(*key);
            assert_eq!(parse_key(&name), Some(*key), "{key:?} -> {name:?}");
            // Names must be something a chord can hold.
            assert!(
                Chord::parse(&name).is_some(),
                "{key:?} has the name {name:?}, which a chord cannot hold"
            );
        }
    }

    #[test]
    fn events_become_chords_per_platform() {
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        assert_eq!(
            event_chord(Key::Enter, cmd_shift, true),
            Chord::parse("Cmd+Shift+Enter").unwrap()
        );
        // Control on macOS is its own key; elsewhere egui reports it as command too.
        assert_eq!(
            event_chord(Key::Space, Modifiers::CTRL, true),
            Chord::parse("Ctrl+Space").unwrap()
        );
        let ctrl = Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            event_chord(Key::Space, ctrl, false).normalized(false),
            Chord::parse("Ctrl+Space").unwrap().normalized(false)
        );
    }

    #[test]
    fn lookup_finds_editor_actions_only() {
        let keymap = UiKeymap::default_for(true);
        assert_eq!(
            keymap.lookup(Key::F, Modifiers::COMMAND),
            Some(ActionId::Find)
        );
        assert_eq!(
            keymap.lookup(Key::F, Modifiers::COMMAND | Modifiers::ALT),
            Some(ActionId::Replace)
        );
        assert_eq!(
            keymap.lookup(Key::Z, Modifiers::COMMAND | Modifiers::SHIFT),
            Some(ActionId::Redo)
        );
        // Run is the app's, not the editor's.
        assert_eq!(keymap.lookup(Key::Enter, Modifiers::COMMAND), None);
        assert_eq!(keymap.lookup(Key::F, Modifiers::NONE), None);
    }

    #[test]
    fn a_remapped_chord_replaces_the_old_one() {
        let mut keymap = Keymap::preset("sqlr").unwrap();
        keymap.set_chords(ActionId::Find, vec![Chord::parse("Cmd+Alt+J").unwrap()]);
        let ui = UiKeymap::new(keymap, true);
        assert_eq!(ui.lookup(Key::F, Modifiers::COMMAND), None);
        assert_eq!(
            ui.lookup(Key::J, Modifiers::COMMAND | Modifiers::ALT),
            Some(ActionId::Find)
        );
        assert_eq!(ui.label(ActionId::Find).as_deref(), Some("⌥⌘J"));
    }
}
