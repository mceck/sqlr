//! Inter (UI text) and the Phosphor icons (MIT).

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// Font data and family names.
pub const INTER: &str = "Inter-Regular";
pub const INTER_SEMIBOLD: &str = "Inter-SemiBold";
pub const ICONS: &str = "phosphor";

static INTER_TTF: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
static INTER_SEMIBOLD_TTF: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");

/// egui's default monospace font and Inter first in `Proportional`, a
/// `FontFamily::Name(INTER_SEMIBOLD)` family for headings, a
/// `FontFamily::Name(ICONS)` family with Phosphor first (Inter has glyphs of its own at some
/// of Phosphor's private-use code points), and `extra_fallbacks` (name, bytes) appended to
/// every family. All bytes are borrowed, never copied.
pub fn definitions(extra_fallbacks: &[(&'static str, &'static [u8])]) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    for (name, bytes) in [
        (INTER, INTER_TTF),
        (INTER_SEMIBOLD, INTER_SEMIBOLD_TTF),
        (ICONS, egui_phosphor::Variant::Regular.font_bytes()),
    ] {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, INTER.into());
    let mut icons = proportional.clone();
    icons.insert(0, ICONS.into());
    fonts.families.insert(FontFamily::Name(ICONS.into()), icons);
    let mut semibold = fonts.families[&FontFamily::Proportional].clone();
    semibold.insert(0, INTER_SEMIBOLD.into());
    fonts
        .families
        .insert(FontFamily::Name(INTER_SEMIBOLD.into()), semibold);

    for &(name, bytes) in extra_fallbacks {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
        for family in fonts.families.values_mut() {
            family.push(name.into());
        }
    }
    fonts
}

/// Installs [`definitions`]; they take effect at the start of the next pass.
pub fn install_fonts(ctx: &egui::Context, extra_fallbacks: &[(&'static str, &'static [u8])]) {
    ctx.set_fonts(definitions(extra_fallbacks));
}

/// The SemiBold font at `size`, or the regular proportional font while it is not installed
/// (so a context without [`install_fonts`] never names an unknown family).
pub fn semibold(ctx: &egui::Context, size: f32) -> egui::FontId {
    named_or_proportional(ctx, INTER_SEMIBOLD, size)
}

/// The icon font at `size` (falls back like [`semibold`]).
pub fn icons(ctx: &egui::Context, size: f32) -> egui::FontId {
    named_or_proportional(ctx, ICONS, size)
}

fn named_or_proportional(ctx: &egui::Context, name: &str, size: f32) -> egui::FontId {
    let family = FontFamily::Name(name.into());
    if has_family(ctx, &family) {
        egui::FontId::new(size, family)
    } else {
        egui::FontId::proportional(size)
    }
}

pub(crate) fn has_family(ctx: &egui::Context, family: &FontFamily) -> bool {
    ctx.fonts(|f| f.definitions().families.contains_key(family))
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    #[test]
    fn bundled_fonts_are_borrowed() {
        let fonts = definitions(&[]);
        for name in [INTER, INTER_SEMIBOLD, ICONS] {
            assert!(
                matches!(fonts.font_data[name].font, Cow::Borrowed(_)),
                "{name} must be borrowed"
            );
        }
    }

    #[test]
    fn bundled_files_are_truetype() {
        for bytes in [INTER_TTF, INTER_SEMIBOLD_TTF] {
            assert_eq!(&bytes[..4], &[0, 1, 0, 0], "a TrueType sfnt header");
        }
    }
}
