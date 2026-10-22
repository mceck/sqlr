//! Bound the native icon's backing pixels; the source artwork remains full resolution.
use std::sync::{Arc, OnceLock};
pub fn window_icon() -> Option<Arc<egui::IconData>> {
    static ICON: OnceLock<Option<Arc<egui::IconData>>> = OnceLock::new();
    ICON.get_or_init(|| {
        let image = image::load_from_memory(include_bytes!("../../../docs/media/icon.png")).ok()?;
        let image = image
            .resize(256, 256, image::imageops::FilterType::Lanczos3)
            .into_rgba8();
        Some(Arc::new(egui::IconData {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        }))
    })
    .clone()
}
#[cfg(target_os = "macos")]
pub(crate) fn native_icon_png() -> Option<Vec<u8>> {
    let icon = window_icon()?;
    let image = image::RgbaImage::from_raw(icon.width, icon.height, icon.rgba.clone())?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .ok()?;
    Some(bytes.into_inner())
}
