//! The integrated terminal: a real PTY, a vt100 parser and an egui panel.
//!
//! The crate is a leaf: the application gives it the theme and font and owns visibility.
//! Terminal output and the shell command never leave it for a log.

pub mod input;
pub mod pty;
pub mod screen;
pub mod session;
pub mod shell;
pub mod widget;

pub use screen::{CellView, CursorShape, Emulator, ScreenSnapshot, TermColor};
pub use session::{BackendFactory, BackendParts, SessionConfig, TerminalSession, TerminalStatus};
pub use widget::{Selection, TerminalAction, TerminalPanel, TerminalTheme};
