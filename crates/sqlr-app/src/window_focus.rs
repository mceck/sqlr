//! Keeps egui's window focus in step with AppKit.
//!
//! On macOS egui-winit does not trust the value of winit's focus event and asks the window
//! whether it is key when the event arrives (winit#4371). A window that becomes key only
//! after that event, as on the first launch of a newly installed bundle while macOS checks
//! it, leaves egui believing it has no focus: text fields draw no caret, and
//! `Response::has_focus` is false for every widget. The frame hook corrects the flag.

/// Marks `raw` focused when the app's window is key but egui was told otherwise.
pub fn correct(raw: &mut egui::RawInput, window_is_key: bool) {
    if window_is_key {
        raw.focused = true;
    }
}

/// Corrects `raw` from the window AppKit reports as key; does nothing elsewhere.
pub fn hook(raw: &mut egui::RawInput) {
    #[cfg(target_os = "macos")]
    if !raw.focused {
        correct(raw, crate::native_menu::main_window_is_key());
    }
    #[cfg(not(target_os = "macos"))]
    let _ = raw;
}

#[cfg(test)]
mod tests {
    use super::correct;

    #[test]
    fn a_key_window_is_focused_even_when_egui_was_told_otherwise() {
        let mut raw = egui::RawInput {
            focused: false,
            ..Default::default()
        };
        correct(&mut raw, true);
        assert!(raw.focused);
    }

    #[test]
    fn a_window_that_is_not_key_keeps_what_egui_was_told() {
        for told in [false, true] {
            let mut raw = egui::RawInput {
                focused: told,
                ..Default::default()
            };
            correct(&mut raw, false);
            assert_eq!(raw.focused, told);
        }
    }
}
