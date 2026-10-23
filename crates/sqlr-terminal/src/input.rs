//! Keystrokes and pastes as bytes for the PTY.

use egui::{Key, Modifiers};

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

/// Pasted text, bracketed when the application asked for it.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\n").replace('\n', "\r");
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
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
