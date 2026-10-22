//! The bundled fonts (Inter, Phosphor; see `sqlr_ui::theme::fonts`) plus a
//! system CJK fallback after them (spec §3.1 item 2).
//!
//! egui's defaults stay as further fallbacks and include an emoji font (Noto Emoji); the system
//! color-emoji fonts are bitmap formats egui cannot render, so only CJK is added here.

use std::sync::OnceLock;

use sqlr_ui::theme;

#[cfg(target_os = "macos")]
const FALLBACKS: &[&str] = &["/System/Library/Fonts/Hiragino Sans GB.ttc"];
#[cfg(target_os = "windows")]
const FALLBACKS: &[&str] = &[r"C:\Windows\Fonts\msyh.ttc"];
#[cfg(target_os = "linux")]
const FALLBACKS: &[&str] = &["/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"];
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
const FALLBACKS: &[&str] = &[];

/// The fallback files that exist, named by their path, with their bytes. Each is read once per
/// process and kept for its lifetime, so egui borrows the bytes instead of holding its own
/// copies (a CJK collection is about 23 MB). Missing or unreadable files are skipped.
fn fallback_fonts() -> &'static [(&'static str, &'static [u8])] {
    static LOADED: OnceLock<Vec<(&'static str, &'static [u8])>> = OnceLock::new();
    LOADED.get_or_init(|| {
        FALLBACKS
            .iter()
            .filter_map(|&path| match std::fs::read(path) {
                Ok(bytes) => Some((path, &*Box::leak(bytes.into_boxed_slice()))),
                Err(_) => {
                    tracing::debug!("fallback font {path} not available");
                    None
                }
            })
            .collect()
    })
}

/// Installs the bundled fonts with every available fallback appended to each family.
pub fn install(ctx: &egui::Context) {
    theme::install_fonts(ctx, fallback_fonts());
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use egui::FontFamily;

    use super::*;

    fn definitions() -> egui::FontDefinitions {
        theme::fonts::definitions(fallback_fonts())
    }

    #[test]
    fn fallback_fonts_are_read_once_and_borrowed() {
        let first = definitions();
        let second = definitions();
        let fallbacks: Vec<&String> = first
            .font_data
            .keys()
            .filter(|k| FALLBACKS.contains(&k.as_str()))
            .collect();
        if cfg!(target_os = "macos") {
            assert!(!fallbacks.is_empty(), "the macOS CJK font must load");
        }
        for name in fallbacks {
            let (a, b) = (&first.font_data[name].font, &second.font_data[name].font);
            assert!(
                matches!(a, Cow::Borrowed(_)),
                "{name} must be borrowed, not copied"
            );
            assert_eq!(a.as_ptr(), b.as_ptr(), "{name} must be read only once");
            // After the bundled fonts, in both families.
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                let list = &first.families[&family];
                assert_eq!(list.last(), Some(name), "{family:?}");
                assert_ne!(list.first(), Some(name), "{family:?}");
            }
        }
    }
}
