//! Maps key presses to editor actions. "Word" motions use ⌥ on macOS and Ctrl elsewhere;
//! ⌘ is egui's `command` (Ctrl off macOS).

use egui::{Key, Modifiers};

use crate::editor::model::{EditCommand, Motion};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    Edit(EditCommand),
    /// Ctrl+Space: opens the completion popup, or closes it when it is open.
    ToggleCompletion,
    OpenFind {
        replace: bool,
    },
    FindNext,
    FindPrevious,
    FormatSql,
    Copy,
    Cut,
    Paste,
    /// Closes the popup, else the find bar, else the extra carets.
    Escape,
}

/// The action for `key` pressed with `m`; `None` when the editor leaves the key alone (the
/// app owns ⌘↵, ⌘P, ⌘⇧P and ⌘T). `page` is the number of lines a page moves.
pub(crate) fn key_action(key: Key, m: Modifiers, mac: bool, page: usize) -> Option<Action> {
    use EditCommand as C;
    let word = if mac { m.alt } else { m.ctrl };
    let mv = |motion| {
        Some(Action::Edit(C::Move {
            motion,
            select: m.shift,
        }))
    };
    match key {
        Key::Enter | Key::Tab => None,
        Key::Backspace if word => Some(Action::Edit(C::DeleteWordBack)),
        Key::Backspace => Some(Action::Edit(C::Backspace)),
        Key::Delete => Some(Action::Edit(C::Delete)),
        Key::Escape => Some(Action::Escape),
        Key::ArrowLeft if mac && m.command => mv(Motion::LineStart),
        Key::ArrowRight if mac && m.command => mv(Motion::LineEnd),
        Key::ArrowLeft if word => mv(Motion::WordLeft),
        Key::ArrowRight if word => mv(Motion::WordRight),
        Key::ArrowLeft => mv(Motion::Left),
        Key::ArrowRight => mv(Motion::Right),
        Key::ArrowUp if m.command && m.alt => Some(Action::Edit(C::AddCaretAbove)),
        Key::ArrowDown if m.command && m.alt => Some(Action::Edit(C::AddCaretBelow)),
        Key::ArrowUp if mac && m.command => mv(Motion::DocStart),
        Key::ArrowDown if mac && m.command => mv(Motion::DocEnd),
        Key::ArrowUp => mv(Motion::Up),
        Key::ArrowDown => mv(Motion::Down),
        Key::Home if !mac && m.command => mv(Motion::DocStart),
        Key::End if !mac && m.command => mv(Motion::DocEnd),
        Key::Home => mv(Motion::LineStart),
        Key::End => mv(Motion::LineEnd),
        Key::PageUp => mv(Motion::PageUp(page)),
        Key::PageDown => mv(Motion::PageDown(page)),
        _ => None,
    }
}

pub(crate) fn mapped_action(action: Option<sqlr_core::ActionId>) -> Option<Action> {
    use sqlr_core::ActionId as A;
    Some(match action? {
        A::Newline => Action::Edit(EditCommand::Newline),
        A::Indent => Action::Edit(EditCommand::Tab),
        A::Outdent => Action::Edit(EditCommand::BackTab),
        A::Copy => Action::Copy,
        A::Cut => Action::Cut,
        A::Paste => Action::Paste,
        A::DuplicateLine => Action::Edit(EditCommand::DuplicateLine),
        A::FormatSql => Action::FormatSql,
        A::Undo => Action::Edit(EditCommand::Undo),
        A::Redo => Action::Edit(EditCommand::Redo),
        A::SelectAll => Action::Edit(EditCommand::SelectAll),
        A::ToggleComment => Action::Edit(EditCommand::ToggleComment),
        A::Find => Action::OpenFind { replace: false },
        A::Replace => Action::OpenFind { replace: true },
        A::FindNext => Action::FindNext,
        A::FindPrevious => Action::FindPrevious,
        A::Complete => Action::ToggleCompletion,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(key: Key, m: Modifiers, mac: bool) -> Option<Action> {
        mapped_action(crate::keymap::UiKeymap::default_for(mac).lookup(key, m))
            .or_else(|| key_action(key, m, mac, 10))
    }

    fn mv(motion: Motion, select: bool) -> Action {
        Action::Edit(EditCommand::Move { motion, select })
    }

    #[test]
    fn word_modifier_depends_on_platform() {
        assert_eq!(
            act(Key::ArrowLeft, Modifiers::ALT, true),
            Some(mv(Motion::WordLeft, false))
        );
        assert_eq!(
            act(Key::ArrowLeft, Modifiers::ALT, false),
            Some(mv(Motion::Left, false))
        );
        assert_eq!(
            act(Key::ArrowRight, Modifiers::CTRL, false),
            Some(mv(Motion::WordRight, false))
        );
        assert_eq!(
            act(Key::Backspace, Modifiers::ALT, true),
            Some(Action::Edit(EditCommand::DeleteWordBack))
        );
        assert_eq!(
            act(Key::ArrowLeft, Modifiers::COMMAND | Modifiers::SHIFT, true),
            Some(mv(Motion::LineStart, true))
        );
        assert_eq!(
            act(Key::Home, Modifiers::COMMAND, false),
            Some(mv(Motion::DocStart, false))
        );
        assert_eq!(
            act(Key::ArrowDown, Modifiers::COMMAND, true),
            Some(mv(Motion::DocEnd, false))
        );
        assert_eq!(
            act(Key::ArrowUp, Modifiers::COMMAND | Modifiers::ALT, true),
            Some(Action::Edit(EditCommand::AddCaretAbove))
        );
        assert_eq!(
            act(Key::PageDown, Modifiers::SHIFT, true),
            Some(mv(Motion::PageDown(10), true))
        );
    }

    #[test]
    fn app_shortcuts_are_left_alone() {
        for k in [Key::Enter, Key::P, Key::K, Key::T] {
            assert_eq!(act(k, Modifiers::COMMAND, true), None, "{k:?}");
        }
        assert_eq!(
            act(Key::Enter, Modifiers::COMMAND | Modifiers::SHIFT, true),
            None
        );
        assert_eq!(act(Key::Q, Modifiers::NONE, true), None);
    }

    #[test]
    fn command_shortcuts() {
        let cmd = Modifiers::COMMAND;
        assert_eq!(
            act(Key::Z, cmd | Modifiers::SHIFT, true),
            Some(Action::Edit(EditCommand::Redo))
        );
        assert_eq!(
            act(Key::Y, cmd, true),
            Some(Action::Edit(EditCommand::Redo))
        );
        assert_eq!(
            act(Key::F, cmd | Modifiers::ALT, true),
            Some(Action::OpenFind { replace: true })
        );
        assert_eq!(
            act(Key::G, cmd | Modifiers::SHIFT, true),
            Some(Action::FindPrevious)
        );
        assert_eq!(
            act(Key::Space, Modifiers::CTRL, true),
            Some(Action::ToggleCompletion)
        );
    }
}
