//! The bundled fonts (Inter, Phosphor; see `sqlr_ui::theme::fonts`) plus a
//! system CJK fallback after them (spec §3.1 item 2).
//!
//! egui's defaults stay as further fallbacks and include an emoji font (Noto Emoji); the system
//! color-emoji fonts are bitmap formats egui cannot render, so only CJK is added here.

use parking_lot::Mutex;
use std::sync::{Arc, OnceLock};

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

/// Installs the small bundled fonts immediately. The system CJK collection is read on a
/// worker only after CJK text is actually painted, including text in popups and results.
pub fn install(ctx: &egui::Context) {
    theme::install_fonts(ctx, &[]);
    enum Load {
        Idle,
        Loading,
        Ready,
        Installed,
        Failed,
    }
    let state = Arc::new(Mutex::new(Load::Idle));
    ctx.on_end_pass(
        "sqlr-cjk-fallback",
        Arc::new(move |ui| {
            let ctx = ui.ctx();
            let mut load = state.lock();
            match *load {
                Load::Installed | Load::Loading | Load::Failed => return,
                Load::Ready => {
                    *load = Load::Installed;
                    drop(load);
                    let families =
                        ctx.fonts(|f| f.definitions().families.keys().cloned().collect::<Vec<_>>());
                    for &(name, bytes) in fallback_fonts() {
                        ctx.add_font(egui::epaint::text::FontInsert::new(
                            name,
                            egui::FontData::from_static(bytes),
                            families
                                .iter()
                                .cloned()
                                .map(|family| egui::epaint::text::InsertFontFamily {
                                    family,
                                    priority: egui::epaint::text::FontPriority::Lowest,
                                })
                                .collect(),
                        ));
                    }
                    ctx.request_repaint();
                    return;
                }
                Load::Idle => {}
            }
            let mut layers = ctx.memory(|m| m.layer_ids().collect::<Vec<_>>());
            layers.extend([
                ui.layer_id(),
                egui::LayerId::background(),
                egui::LayerId::debug(),
            ]);
            let needed = ctx.graphics(|g| {
                layers.iter().any(|&id| {
                    g.get(id).is_some_and(|list| {
                        list.all_entries()
                            .any(|entry| shape_needs_cjk(&entry.shape))
                    })
                })
            });
            if !needed {
                return;
            }
            *load = Load::Loading;
            drop(load);
            let state = state.clone();
            let repaint = ctx.clone();
            std::thread::Builder::new()
                .name("sqlr-cjk-font".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(fallback_fonts);
                    *state.lock() = if result.is_ok() {
                        Load::Ready
                    } else {
                        Load::Failed
                    };
                    repaint.request_repaint();
                })
                .expect("spawn CJK font loader");
        }),
    );
}

fn shape_needs_cjk(shape: &egui::Shape) -> bool {
    match shape {
        egui::Shape::Text(t) => t.galley.text().chars().any(|c| {
            matches!(c as u32,
            0x1100..=0x11ff | 0x2e80..=0xa4cf | 0xac00..=0xd7ff |
            0xf900..=0xfaff | 0xfe30..=0xfe4f | 0xff00..=0xffef | 0x20000..=0x323af)
        }),
        egui::Shape::Vec(shapes) => shapes.iter().any(shape_needs_cjk),
        _ => false,
    }
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
    fn latin_frames_do_not_install_the_cjk_fallback() {
        let ctx = egui::Context::default();
        install(&ctx);
        for _ in 0..3 {
            let mut out = ctx.run_ui(Default::default(), |ui| {
                ui.label("SELECT hello, café");
            });
            out.textures_delta.clear();
        }
        ctx.fonts(|fonts| {
            assert!(
                !fonts
                    .definitions()
                    .font_data
                    .keys()
                    .any(|k| FALLBACKS.contains(&k.as_str()))
            );
        });
    }

    #[test]
    fn visible_cjk_text_loads_the_fallback_in_the_background() {
        if !FALLBACKS.iter().any(|p| std::path::Path::new(p).exists()) {
            return;
        }
        let ctx = egui::Context::default();
        install(&ctx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut out = ctx.run_ui(Default::default(), |ui| {
                ui.label("日本語 中文");
            });
            out.textures_delta.clear();
            if ctx.fonts(|f| {
                f.definitions()
                    .font_data
                    .keys()
                    .any(|k| FALLBACKS.contains(&k.as_str()))
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "CJK fallback did not load"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
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
