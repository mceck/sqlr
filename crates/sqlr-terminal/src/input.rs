//! Keystrokes, pastes and mouse reports as bytes for the PTY.

use egui::{Key, Modifiers, PointerButton};

use crate::screen::MouseProtocolEncoding;

/// The bytes one key press sends, or `None` when the key is a shortcut the application
/// consumes (copy, paste, the terminal toggle).
pub fn encode_key(
    key: Key,
    modifiers: Modifiers,
    application_cursor: bool,
    mac: bool,
) -> Option<Vec<u8>> {
    // Cmd (⌘) is the application's shortcut modifier on macOS; never send it to the shell.
    if modifiers.command && mac {
        return None;
    }
    if modifiers.ctrl && !modifiers.command && !mac && modifiers.shift {
        // Ctrl+Shift+C/V are copy/paste off macOS; the widget handles them before this.
        if matches!(key, Key::C | Key::V) {
            return None;
        }
    }
    let cursor = if application_cursor { b'O' } else { b'[' };
    let special: Option<&[u8]> = match key {
        Key::Enter => Some(b"\r"),
        Key::Tab => Some(b"\t"),
        Key::Backspace => Some(b"\x7f"),
        Key::Escape => Some(b"\x1b"),
        Key::ArrowUp => Some(match cursor {
            b'O' => b"\x1bOA",
            _ => b"\x1b[A",
        }),
        Key::ArrowDown => Some(match cursor {
            b'O' => b"\x1bOB",
            _ => b"\x1b[B",
        }),
        Key::ArrowRight => Some(match cursor {
            b'O' => b"\x1bOC",
            _ => b"\x1b[C",
        }),
        Key::ArrowLeft => Some(match cursor {
            b'O' => b"\x1bOD",
            _ => b"\x1b[D",
        }),
        Key::Home => Some(b"\x1b[H"),
        Key::End => Some(b"\x1b[F"),
        Key::Insert => Some(b"\x1b[2~"),
        Key::Delete => Some(b"\x1b[3~"),
        Key::PageUp => Some(b"\x1b[5~"),
        Key::PageDown => Some(b"\x1b[6~"),
        Key::F1 => Some(b"\x1bOP"),
        Key::F2 => Some(b"\x1bOQ"),
        Key::F3 => Some(b"\x1bOR"),
        Key::F4 => Some(b"\x1bOS"),
        Key::F5 => Some(b"\x1b[15~"),
        Key::F6 => Some(b"\x1b[17~"),
        Key::F7 => Some(b"\x1b[18~"),
        Key::F8 => Some(b"\x1b[19~"),
        Key::F9 => Some(b"\x1b[20~"),
        Key::F10 => Some(b"\x1b[21~"),
        Key::F11 => Some(b"\x1b[23~"),
        Key::F12 => Some(b"\x1b[24~"),
        _ => None,
    };
    if let Some(bytes) = special {
        return Some(bytes.to_vec());
    }
    let ctrl = modifiers.ctrl && !(mac && modifiers.command);
    if ctrl {
        let byte = control_byte(key)?;
        let mut out = Vec::new();
        if modifiers.alt {
            out.push(0x1b);
        }
        out.push(byte);
        return Some(out);
    }
    let ch = key_char(key, modifiers.shift);
    let ch = ch?;
    let mut out = Vec::new();
    if modifiers.alt {
        out.push(0x1b);
    }
    let mut buffer = [0; 4];
    out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
    Some(out)
}

/// Ctrl+letter is `letter & 0x1f`; Ctrl+Space is NUL; the remaining punctuation follows
/// the xterm table.
fn control_byte(key: Key) -> Option<u8> {
    Some(match key {
        Key::A => 0x01,
        Key::B => 0x02,
        Key::C => 0x03,
        Key::D => 0x04,
        Key::E => 0x05,
        Key::F => 0x06,
        Key::G => 0x07,
        Key::H => 0x08,
        Key::I => 0x09,
        Key::J => 0x0a,
        Key::K => 0x0b,
        Key::L => 0x0c,
        Key::M => 0x0d,
        Key::N => 0x0e,
        Key::O => 0x0f,
        Key::P => 0x10,
        Key::Q => 0x11,
        Key::R => 0x12,
        Key::S => 0x13,
        Key::T => 0x14,
        Key::U => 0x15,
        Key::V => 0x16,
        Key::W => 0x17,
        Key::X => 0x18,
        Key::Y => 0x19,
        Key::Z => 0x1a,
        Key::Space => 0x00,
        Key::Backslash => 0x1c,
        Key::CloseBracket => 0x1d,
        Key::Minus => 0x1f,
        Key::Slash => 0x1f,
        Key::Questionmark => 0x7f,
        Key::OpenBracket => 0x1b,
        _ => return None,
    })
}

fn key_char(key: Key, shift: bool) -> Option<char> {
    let c = match key {
        Key::A => 'a',
        Key::B => 'b',
        Key::C => 'c',
        Key::D => 'd',
        Key::E => 'e',
        Key::F => 'f',
        Key::G => 'g',
        Key::H => 'h',
        Key::I => 'i',
        Key::J => 'j',
        Key::K => 'k',
        Key::L => 'l',
        Key::M => 'm',
        Key::N => 'n',
        Key::O => 'o',
        Key::P => 'p',
        Key::Q => 'q',
        Key::R => 'r',
        Key::S => 's',
        Key::T => 't',
        Key::U => 'u',
        Key::V => 'v',
        Key::W => 'w',
        Key::X => 'x',
        Key::Y => 'y',
        Key::Z => 'z',
        Key::Num0 => '0',
        Key::Num1 => '1',
        Key::Num2 => '2',
        Key::Num3 => '3',
        Key::Num4 => '4',
        Key::Num5 => '5',
        Key::Num6 => '6',
        Key::Num7 => '7',
        Key::Num8 => '8',
        Key::Num9 => '9',
        Key::Space => ' ',
        Key::Comma => ',',
        Key::Period => '.',
        Key::Slash => '/',
        Key::Backslash => '\\',
        Key::Semicolon => ';',
        Key::Colon => ':',
        Key::Quote => '\'',
        Key::OpenBracket => '[',
        Key::CloseBracket => ']',
        Key::Minus => '-',
        Key::Plus | Key::Equals => '=',
        Key::Backtick => '`',
        Key::Questionmark => '?',
        Key::Exclamationmark => '!',
        Key::Pipe => '|',
        Key::OpenCurlyBracket => '{',
        Key::CloseCurlyBracket => '}',
        _ => return None,
    };
    if shift {
        Some(match c {
            'a'..='z' => c.to_ascii_uppercase(),
            _ => c,
        })
    } else {
        Some(c)
    }
}

/// The PTY input one frame of egui events produces.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct EncodedInput {
    pub(crate) bytes: Vec<u8>,
    pub(crate) copy: bool,
}

/// Turns a frame of egui events into PTY bytes.
///
/// A printable key arrives twice: as [`egui::Event::Key`] and as the [`egui::Event::Text`] it
/// produced. Only the text is sent, which also follows the keyboard layout and dead keys;
/// encoding the key as well would send every character twice.
pub(crate) fn encode_events(
    events: &[egui::Event],
    application_cursor: bool,
    bracketed_paste: bool,
    kitty_flags: u16,
    mac: bool,
) -> EncodedInput {
    let mut out = EncodedInput::default();
    let mut alt = false;
    // A key encoded as CSI u must not also send the text it produced (winit sends both for
    // Alt chords).
    let mut skip_text = false;
    // Only the enhancements we implement: disambiguate and alternate keys.
    let kitty = kitty_flags & (KITTY_DISAMBIGUATE | KITTY_ALTERNATE);
    for event in events {
        match event {
            egui::Event::Key {
                key,
                modifiers,
                pressed: true,
                ..
            } => {
                alt = false;
                skip_text = false;
                if *key == Key::Copy || (is_copy(*modifiers, mac) && *key == Key::C) {
                    out.copy = true;
                } else if *key == Key::Paste || (is_paste(*modifiers, mac) && *key == Key::V) {
                    // egui turns the platform paste into Event::Paste below.
                } else if kitty != 0 {
                    // The application asked for unambiguous key events: Cmd (super), Alt
                    // chords and the functional keys go out as CSI u.
                    if let Some(bytes) = encode_kitty_key(*key, *modifiers, kitty, mac) {
                        skip_text = is_text_key(*key);
                        out.bytes.extend(bytes);
                    } else if !is_text_key(*key) {
                        // Enter, Tab and Backspace keep their legacy bytes.
                        if let Some(bytes) = encode_key(*key, *modifiers, application_cursor, mac) {
                            out.bytes.extend(bytes);
                        }
                    }
                } else if is_text_key(*key) && !modifiers.ctrl && !modifiers.command {
                    // The character itself arrives as Event::Text; decide the Alt prefix now.
                    alt = modifiers.alt;
                } else if let Some(bytes) = encode_key(*key, *modifiers, application_cursor, mac) {
                    out.bytes.extend(bytes);
                }
            }
            egui::Event::Text(text) => {
                if skip_text {
                    skip_text = false;
                    continue;
                }
                if alt {
                    out.bytes.push(0x1b);
                    alt = false;
                }
                out.bytes.extend_from_slice(text.as_bytes());
            }
            egui::Event::Paste(text) => {
                out.bytes.extend(encode_paste(text, bracketed_paste));
            }
            egui::Event::Copy => out.copy = true,
            _ => {}
        }
    }
    out
}

/// Keys that produce their character through [`egui::Event::Text`].
fn is_text_key(key: Key) -> bool {
    key_char(key, false).is_some()
}

/// Pasted text, bracketed when the application asked for it.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\n").replace('\n', "\r");
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

const KITTY_DISAMBIGUATE: u16 = 0b0001;
const KITTY_ALTERNATE: u16 = 0b0100;

/// The unambiguous key encoding the kitty keyboard protocol enables. `None` when the key
/// still goes out as text or as its legacy bytes.
pub(crate) fn encode_kitty_key(
    key: Key,
    modifiers: Modifiers,
    flags: u16,
    mac: bool,
) -> Option<Vec<u8>> {
    let bits = u8::from(modifiers.shift)
        + u8::from(modifiers.alt) * 2
        + u8::from(modifiers.ctrl) * 4
        + u8::from(mac && modifiers.command) * 8;
    // Shift alone still produces text; Alt, Ctrl and Cmd are unambiguous chords.
    let chorded = bits & 0b1110 != 0;
    match key {
        // Escape is never legacy while disambiguating.
        Key::Escape => return Some(csi_u(27, bits, None)),
        // Enter, Tab and Backspace keep their legacy bytes while unmodified, so `reset`
        // still works when an application crashes; modified chords are unambiguous.
        Key::Enter | Key::Tab | Key::Backspace => {
            if bits == 0 {
                return None;
            }
            let code = match key {
                Key::Enter => 13,
                Key::Tab => 9,
                Key::Backspace => 127,
                _ => unreachable!(),
            };
            return Some(csi_u(code, bits, None));
        }
        _ => {}
    }
    if let Some((number, final_byte)) = functional_key(key) {
        return Some(csi_functional(number, bits, final_byte));
    }
    if is_text_key(key) && chorded {
        let base = u32::from(key_char(key, false)?);
        let shifted = if flags & KITTY_ALTERNATE != 0 && modifiers.shift {
            key_char(key, true)
        } else {
            None
        };
        return Some(csi_u(base, bits, shifted));
    }
    None
}

/// `CSI unicode-key-code[:shifted]; modifiers u`.
fn csi_u(code: u32, bits: u8, shifted: Option<char>) -> Vec<u8> {
    let mut out = format!("\x1b[{code}");
    if let Some(shifted) = shifted {
        out.push(':');
        out.push_str(&u32::from(shifted).to_string());
    }
    out.push(';');
    out.push_str(&(u16::from(bits) + 1).to_string());
    out.push('u');
    out.into_bytes()
}

/// The legacy functional form with an explicit modifier: `CSI number; modifiers final`.
fn csi_functional(number: u8, bits: u8, final_byte: char) -> Vec<u8> {
    format!("\x1b[{number};{}{final_byte}", u16::from(bits) + 1).into_bytes()
}

fn functional_key(key: Key) -> Option<(u8, char)> {
    Some(match key {
        Key::ArrowUp => (1, 'A'),
        Key::ArrowDown => (1, 'B'),
        Key::ArrowRight => (1, 'C'),
        Key::ArrowLeft => (1, 'D'),
        Key::Home => (1, 'H'),
        Key::End => (1, 'F'),
        Key::F1 => (1, 'P'),
        Key::F2 => (1, 'Q'),
        Key::F3 => (1, 'R'),
        Key::F4 => (1, 'S'),
        Key::Insert => (2, '~'),
        Key::Delete => (3, '~'),
        Key::PageUp => (5, '~'),
        Key::PageDown => (6, '~'),
        Key::F5 => (15, '~'),
        Key::F6 => (17, '~'),
        Key::F7 => (18, '~'),
        Key::F8 => (19, '~'),
        Key::F9 => (20, '~'),
        Key::F10 => (21, '~'),
        Key::F11 => (23, '~'),
        Key::F12 => (24, '~'),
        _ => return None,
    })
}

/// A mouse action for an application that enabled mouse tracking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MouseEvent {
    Press(PointerButton),
    Release(PointerButton),
    /// The pointer moved; the button held down, if any.
    Move(Option<PointerButton>),
    Wheel {
        up: bool,
    },
}

/// Encodes a mouse report at a 1-based cell for the protocol the application chose.
pub(crate) fn encode_mouse(
    event: MouseEvent,
    col: u16,
    row: u16,
    modifiers: Modifiers,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    // X10 button numbers; wheel buttons are 64 (up) and 65 (down). Motion adds 32.
    let (mut code, release) = match event {
        MouseEvent::Press(button) => (button_code(button)?, false),
        MouseEvent::Release(button) => (button_code(button)?, true),
        MouseEvent::Move(button) => (button.and_then(button_code).unwrap_or(3) + 32, false),
        MouseEvent::Wheel { up } => (if up { 64 } else { 65 }, false),
    };
    let mods =
        u8::from(modifiers.shift) * 4 + u8::from(modifiers.alt) * 8 + u8::from(modifiers.ctrl) * 16;
    // SGR keeps the button in a release and marks it with 'm'; the older encodings
    // report button 3.
    if release && encoding != MouseProtocolEncoding::Sgr {
        code = 3;
    }
    code += mods;
    match encoding {
        MouseProtocolEncoding::Sgr => Some(
            format!(
                "\x1b[<{code};{col};{row}{}",
                if release { 'm' } else { 'M' }
            )
            .into_bytes(),
        ),
        MouseProtocolEncoding::Utf8 => {
            let mut out = b"\x1b[M".to_vec();
            for value in [u32::from(code), u32::from(col), u32::from(row)] {
                let c = char::from_u32(value + 32).unwrap_or('\u{fffd}');
                let mut buffer = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            }
            Some(out)
        }
        MouseProtocolEncoding::Default => {
            let mut out = b"\x1b[M".to_vec();
            out.push(code.saturating_add(32));
            out.push(legacy_coordinate(col));
            out.push(legacy_coordinate(row));
            Some(out)
        }
    }
}

fn button_code(button: PointerButton) -> Option<u8> {
    match button {
        PointerButton::Primary => Some(0),
        PointerButton::Middle => Some(1),
        PointerButton::Secondary => Some(2),
        PointerButton::Extra1 | PointerButton::Extra2 => None,
    }
}

/// The X10 encoding adds 32 to each coordinate and cannot go past one byte.
fn legacy_coordinate(value: u16) -> u8 {
    u8::try_from(value.saturating_add(32)).unwrap_or(u8::MAX)
}

/// Whether this chord is the platform's copy shortcut.
pub fn is_copy(modifiers: Modifiers, mac: bool) -> bool {
    if modifiers.alt {
        return false;
    }
    if mac {
        modifiers.command && !modifiers.ctrl
    } else {
        modifiers.ctrl && modifiers.shift
    }
}

/// Whether this chord is the platform's paste shortcut.
pub fn is_paste(modifiers: Modifiers, mac: bool) -> bool {
    if modifiers.alt {
        return false;
    }
    if mac {
        modifiers.command && !modifiers.ctrl
    } else {
        modifiers.ctrl && modifiers.shift
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> Modifiers {
        Modifiers::NONE
    }

    #[test]
    fn text_and_special_keys() {
        assert_eq!(encode_key(Key::A, m(), false, false), Some(b"a".to_vec()));
        assert_eq!(
            encode_key(Key::A, Modifiers::SHIFT, false, false),
            Some(b"A".to_vec())
        );
        assert_eq!(
            encode_key(Key::Enter, m(), false, false),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            encode_key(Key::ArrowUp, m(), false, false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(Key::ArrowUp, m(), true, false),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode_key(Key::PageDown, m(), false, false),
            Some(b"\x1b[6~".to_vec())
        );
        assert_eq!(
            encode_key(Key::F5, m(), false, false),
            Some(b"\x1b[15~".to_vec())
        );
    }

    #[test]
    fn ctrl_and_alt_combinations() {
        assert_eq!(
            encode_key(Key::C, Modifiers::CTRL, false, false),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(Key::C, Modifiers::CTRL, false, true),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(Key::A, Modifiers::ALT, false, false),
            Some(b"\x1ba".to_vec())
        );
        assert_eq!(
            encode_key(Key::Space, Modifiers::CTRL, false, false),
            Some(vec![0])
        );
        // The application shortcuts are not shell input.
        assert_eq!(encode_key(Key::C, Modifiers::COMMAND, false, true), None);
    }

    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn a_typed_character_is_sent_once() {
        // The integration sends the key press and, right after it, the text it produced.
        let events = [key(Key::A, m()), egui::Event::Text("a".into())];
        assert_eq!(
            encode_events(&events, false, false, 0, false).bytes,
            b"a".to_vec()
        );
        let events = [key(Key::A, Modifiers::SHIFT), egui::Event::Text("A".into())];
        assert_eq!(
            encode_events(&events, false, false, 0, false).bytes,
            b"A".to_vec()
        );
    }

    #[test]
    fn special_and_control_keys_still_encode() {
        assert_eq!(
            encode_events(&[key(Key::Enter, m())], false, false, 0, false).bytes,
            b"\r".to_vec()
        );
        assert_eq!(
            encode_events(&[key(Key::ArrowUp, m())], false, false, 0, false).bytes,
            b"\x1b[A".to_vec()
        );
        assert_eq!(
            encode_events(&[key(Key::C, Modifiers::CTRL)], false, false, 0, false).bytes,
            vec![0x03]
        );
    }

    #[test]
    fn alt_text_keeps_the_escape_prefix() {
        let events = [key(Key::A, Modifiers::ALT), egui::Event::Text("a".into())];
        assert_eq!(
            encode_events(&events, false, false, 0, false).bytes,
            b"\x1ba".to_vec()
        );
    }

    #[test]
    fn copy_and_paste_events_reach_the_panel() {
        let copy = encode_events(&[egui::Event::Copy], false, false, 0, true);
        assert!(copy.copy);
        assert!(copy.bytes.is_empty());
        let paste = encode_events(&[egui::Event::Paste("a\nb".into())], false, true, 0, true);
        assert_eq!(paste.bytes, b"\x1b[200~a\rb\x1b[201~".to_vec());
    }

    #[test]
    fn sgr_mouse_reports_use_one_based_cells() {
        let sgr = |event, col, row, mods| {
            encode_mouse(event, col, row, mods, MouseProtocolEncoding::Sgr).unwrap()
        };
        assert_eq!(
            sgr(MouseEvent::Press(PointerButton::Primary), 5, 2, m()),
            b"\x1b[<0;5;2M".to_vec()
        );
        assert_eq!(
            sgr(MouseEvent::Release(PointerButton::Primary), 5, 2, m()),
            b"\x1b[<0;5;2m".to_vec()
        );
        assert_eq!(
            sgr(
                MouseEvent::Press(PointerButton::Secondary),
                5,
                2,
                Modifiers::CTRL
            ),
            b"\x1b[<18;5;2M".to_vec()
        );
        assert_eq!(
            sgr(MouseEvent::Wheel { up: true }, 5, 2, m()),
            b"\x1b[<64;5;2M".to_vec()
        );
        assert_eq!(
            sgr(MouseEvent::Wheel { up: false }, 5, 2, m()),
            b"\x1b[<65;5;2M".to_vec()
        );
        assert_eq!(
            sgr(MouseEvent::Move(Some(PointerButton::Primary)), 5, 2, m()),
            b"\x1b[<32;5;2M".to_vec()
        );
        assert_eq!(
            sgr(MouseEvent::Move(None), 5, 2, m()),
            b"\x1b[<35;5;2M".to_vec()
        );
    }

    #[test]
    fn legacy_mouse_reports_add_thirty_two() {
        let press = encode_mouse(
            MouseEvent::Wheel { up: true },
            5,
            2,
            Modifiers::NONE,
            MouseProtocolEncoding::Default,
        )
        .unwrap();
        assert_eq!(press, b"\x1b[M`%\"".to_vec());
        let release = encode_mouse(
            MouseEvent::Release(PointerButton::Primary),
            5,
            2,
            Modifiers::NONE,
            MouseProtocolEncoding::Default,
        )
        .unwrap();
        assert_eq!(release, b"\x1b[M#%\"".to_vec());
    }

    #[test]
    fn utf8_mouse_reports_encode_each_value() {
        let bytes = encode_mouse(
            MouseEvent::Press(PointerButton::Middle),
            200,
            2,
            Modifiers::NONE,
            MouseProtocolEncoding::Utf8,
        )
        .unwrap();
        assert_eq!(bytes, "\x1b[M!\u{e8}\"".as_bytes().to_vec());
    }

    #[test]
    fn kitty_disambiguation_sends_super_and_modified_keys_as_csi_u() {
        // Cmd+A on macOS: what opencode's `super+a` keybind needs.
        assert_eq!(
            encode_kitty_key(Key::A, Modifiers::COMMAND, 5, true),
            Some(b"\x1b[97;9u".to_vec())
        );
        // Alternate reporting adds the shifted key for Cmd+Shift+A.
        assert_eq!(
            encode_kitty_key(Key::A, Modifiers::COMMAND.plus(Modifiers::SHIFT), 5, true),
            Some(b"\x1b[97:65;10u".to_vec())
        );
        // Ctrl+I is no longer indistinguishable from Tab.
        assert_eq!(
            encode_kitty_key(Key::I, Modifiers::CTRL, 1, false),
            Some(b"\x1b[105;5u".to_vec())
        );
        // Functional keys carry an explicit modifier.
        assert_eq!(
            encode_kitty_key(Key::ArrowUp, Modifiers::NONE, 1, false),
            Some(b"\x1b[1;1A".to_vec())
        );
        assert_eq!(
            encode_kitty_key(Key::ArrowUp, Modifiers::COMMAND, 1, true),
            Some(b"\x1b[1;9A".to_vec())
        );
        assert_eq!(
            encode_kitty_key(Key::Escape, Modifiers::NONE, 1, false),
            Some(b"\x1b[27;1u".to_vec())
        );
        assert_eq!(
            encode_kitty_key(Key::Tab, Modifiers::SHIFT, 5, true),
            Some(b"\x1b[9;2u".to_vec())
        );
        // Plain text and the reset-friendly keys keep their legacy path.
        assert_eq!(encode_kitty_key(Key::A, Modifiers::SHIFT, 5, true), None);
        assert_eq!(encode_kitty_key(Key::Enter, Modifiers::NONE, 5, true), None);
    }

    #[test]
    fn kitty_mode_keeps_plain_text_and_legacy_enter() {
        let events = [
            key(Key::A, Modifiers::COMMAND),
            egui::Event::Text("a".into()),
        ];
        assert_eq!(
            encode_events(&events, false, false, 5, true).bytes,
            b"\x1b[97;9u".to_vec()
        );
        // Alt chords are CSI u too, so the text winit produced for them is dropped.
        let events = [key(Key::A, Modifiers::ALT), egui::Event::Text("a".into())];
        assert_eq!(
            encode_events(&events, false, false, 5, true).bytes,
            b"\x1b[97;3u".to_vec()
        );
        let events = [key(Key::A, m()), egui::Event::Text("a".into())];
        assert_eq!(encode_events(&events, false, false, 5, true).bytes, b"a");
        let events = [key(Key::Enter, m())];
        assert_eq!(encode_events(&events, false, false, 5, true).bytes, b"\r");
    }

    #[test]
    fn paste_brackets_when_asked() {
        assert_eq!(encode_paste("a\nb", false), b"a\rb".to_vec());
        assert_eq!(
            encode_paste("a\r\nb", true),
            b"\x1b[200~a\rb\x1b[201~".to_vec()
        );
    }

    #[test]
    fn copy_paste_shortcuts_follow_the_platform() {
        assert!(is_copy(Modifiers::COMMAND, true));
        assert!(!is_copy(Modifiers::CTRL, true));
        assert!(is_copy(Modifiers::CTRL.plus(Modifiers::SHIFT), false));
        assert!(!is_copy(Modifiers::CTRL, false));
        assert!(is_paste(Modifiers::COMMAND, true));
        assert!(is_paste(Modifiers::CTRL.plus(Modifiers::SHIFT), false));
    }
}
