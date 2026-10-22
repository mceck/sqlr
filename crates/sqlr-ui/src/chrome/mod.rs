//! The application shell's chrome: toolbar buttons, the connection chip, the editor
//! breadcrumb, status-bar segments, toasts and empty states.

mod breadcrumb;
mod button;
mod chip;
mod empty;
mod status;
mod toast;

pub use breadcrumb::{Crumb, breadcrumb};
pub use button::tool_button;
pub use chip::{ChipAction, ChipItem, ChipState, connection_chip};
pub use empty::empty_state;
pub use status::status_segment;
pub use toast::{ToastKind, Toasts};
