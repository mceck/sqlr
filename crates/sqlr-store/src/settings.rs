use std::path::Path;

use sqlr_core::{Level, SettingValue, ThemePref};

use crate::{LayeredSettings, StoreError};

/// The editor font sizes the settings accept, in points.
pub const MONO_FONT_SIZES: std::ops::RangeInclusive<f32> = 10.0..=24.0;
const DEFAULT_MONO_FONT_SIZE: f32 = 13.0;
/// The rows per fetch the settings accept.
pub const PAGE_SIZES: std::ops::RangeInclusive<usize> = 1..=1_000_000;
/// The connect timeouts the settings accept, in seconds.
pub const CONNECT_TIMEOUTS_S: std::ops::RangeInclusive<u32> = 1..=600;
/// The UI zoom factors the settings accept.
pub const UI_SCALES: std::ops::RangeInclusive<f32> = 0.75..=2.0;
const DEFAULT_UI_SCALE: f32 = 1.0;
/// The minutes a sidecar driver outlives its last connection that the settings accept.
pub const SIDECAR_IDLE_MINUTES: std::ops::RangeInclusive<u32> = 1..=240;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Rows per fetch, within [`PAGE_SIZES`].
    pub page_size: usize,
    /// A script stops at its first failing statement.
    pub stop_script_on_error: bool,
    pub theme: ThemePref,
    /// The editor's (and the grid's) monospace font size, within [`MONO_FONT_SIZES`].
    pub mono_font_size: f32,
    /// Each editor tab runs on its own connection (else all share the explorer's).
    pub session_per_tab: bool,
    /// Seconds allowed for SSH setup plus the driver's connect, within
    /// [`CONNECT_TIMEOUTS_S`].
    pub connect_timeout_s: u32,
    /// The whole UI's zoom factor, within [`UI_SCALES`].
    pub ui_scale: f32,
    /// The editor shows its line-number gutter.
    pub line_numbers: bool,
    /// The editor wraps long lines at the viewport width.
    pub soft_wrap: bool,
    /// "Fetch all" stops at this many rows; 0 means no cap.
    pub row_cap: usize,
    /// Minutes a sidecar driver keeps running after its last connection closes, within
    /// [`SIDECAR_IDLE_MINUTES`].
    pub sidecar_idle_minutes: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            page_size: 500,
            stop_script_on_error: true,
            theme: ThemePref::System,
            mono_font_size: DEFAULT_MONO_FONT_SIZE,
            session_per_tab: true,
            connect_timeout_s: 15,
            ui_scale: DEFAULT_UI_SCALE,
            line_numbers: true,
            soft_wrap: false,
            row_cap: 100_000,
            sidecar_idle_minutes: 5,
        }
    }
}

impl Settings {
    /// The flat snapshot of the resolved values. With `conn` (the connection's id and its
    /// profile's id) the connection-level overrides apply.
    pub fn resolve(layered: &LayeredSettings, conn: Option<(&str, &str)>) -> Self {
        let get = |id: &str| layered.get(id, conn).0;
        Self {
            page_size: get("grid.page_size").as_i64() as usize,
            stop_script_on_error: get("general.stop_script_on_error").as_bool(),
            theme: match get("appearance.theme").as_str() {
                "light" => ThemePref::Light,
                "dark" => ThemePref::Dark,
                _ => ThemePref::System,
            },
            mono_font_size: get("editor.font_size").as_f64() as f32,
            session_per_tab: get("connection.session_per_tab").as_bool(),
            connect_timeout_s: get("connection.connect_timeout_s").as_i64() as u32,
            ui_scale: get("appearance.ui_scale").as_f64() as f32,
            line_numbers: get("editor.line_numbers").as_bool(),
            soft_wrap: get("editor.soft_wrap").as_bool(),
            row_cap: get("grid.row_cap").as_i64() as usize,
            sidecar_idle_minutes: get("drivers.sidecar_idle_minutes").as_i64() as u32,
        }
    }

    /// Writes every field at the global level of `layered`.
    pub fn apply_to(&self, layered: &mut LayeredSettings) {
        let theme = match self.theme {
            ThemePref::System => "system",
            ThemePref::Light => "light",
            ThemePref::Dark => "dark",
        };
        let values = [
            ("grid.page_size", SettingValue::Int(self.page_size as i64)),
            (
                "general.stop_script_on_error",
                SettingValue::Bool(self.stop_script_on_error),
            ),
            ("appearance.theme", SettingValue::Choice(theme.into())),
            (
                "editor.font_size",
                SettingValue::Float(f64::from(self.mono_font_size)),
            ),
            (
                "connection.session_per_tab",
                SettingValue::Bool(self.session_per_tab),
            ),
            (
                "connection.connect_timeout_s",
                SettingValue::Int(i64::from(self.connect_timeout_s)),
            ),
            (
                "appearance.ui_scale",
                SettingValue::Float(f64::from(self.ui_scale)),
            ),
            ("editor.line_numbers", SettingValue::Bool(self.line_numbers)),
            ("editor.soft_wrap", SettingValue::Bool(self.soft_wrap)),
            ("grid.row_cap", SettingValue::Int(self.row_cap as i64)),
            (
                "drivers.sidecar_idle_minutes",
                SettingValue::Int(i64::from(self.sidecar_idle_minutes)),
            ),
        ];
        for (id, value) in values {
            // Every field is in range here (the dialog clamps); a value that is not is
            // skipped and the old one stays.
            let _ = layered.set(id, Level::Global, Some(value));
        }
    }

    /// The global values in `<dir>/settings.toml` (either shape); missing keys have their
    /// defaults and out-of-range values are clamped. An unreadable file is kept as
    /// `.corrupt` and the defaults are returned: use [`LayeredSettings::load`] to learn that.
    pub fn load(dir: &Path) -> Result<Self, StoreError> {
        Ok(Self::resolve(&LayeredSettings::load(dir).0, None))
    }

    /// Stores these values at the global level, keeping every override.
    pub fn save(&self, dir: &Path) -> Result<(), StoreError> {
        let (mut layered, _) = LayeredSettings::load(dir);
        self.apply_to(&mut layered);
        layered.save(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_with(text: &str) -> Settings {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("settings.toml"), text).unwrap();
        Settings::load(dir.path()).unwrap()
    }

    #[test]
    fn load_clamps_the_connect_timeout() {
        // The UI accepts at least one second; the engine silently raises zero to one,
        // so the stored settings would not match what connections actually use.
        let settings = load_with("connect_timeout_s = 0\n");
        assert!(settings.connect_timeout_s >= 1, "{settings:?}");
    }

    #[test]
    fn load_clamps_the_page_size() {
        let settings = load_with("page_size = 0\n");
        assert!(settings.page_size >= 1, "{settings:?}");
    }

    #[test]
    fn load_clamps_oversized_values_to_the_shared_limits() {
        let settings = load_with(
            "page_size = 999999999\nconnect_timeout_s = 4000000000\nmono_font_size = 99.0\n",
        );
        assert_eq!(settings.page_size, *PAGE_SIZES.end());
        assert_eq!(settings.connect_timeout_s, *CONNECT_TIMEOUTS_S.end());
        assert_eq!(settings.mono_font_size, *MONO_FONT_SIZES.end());
        let settings = load_with("connect_timeout_s = 0\npage_size = 1\n");
        assert_eq!(settings.connect_timeout_s, *CONNECT_TIMEOUTS_S.start());
        assert_eq!(settings.page_size, 1);
    }

    #[test]
    fn driver_settings_default_and_clamp() {
        let old = load_with("page_size = 200\n");
        assert_eq!(old.sidecar_idle_minutes, 5);
        let odd = load_with("sidecar_idle_minutes = 0\n");
        assert_eq!(odd.sidecar_idle_minutes, 1);
        assert_eq!(
            load_with("sidecar_idle_minutes = 99999\n").sidecar_idle_minutes,
            240
        );
    }

    #[test]
    fn settings_new_fields_round_trip_and_clamp() {
        // An old file without the new keys keeps their defaults.
        let old = load_with("page_size = 200\n");
        assert_eq!(old.page_size, 200);
        assert_eq!(old.ui_scale, 1.0);
        assert!(old.line_numbers);
        assert!(!old.soft_wrap);
        assert_eq!(old.row_cap, 100_000);

        assert_eq!(load_with("ui_scale = 5\n").ui_scale, *UI_SCALES.end());
        assert_eq!(load_with("ui_scale = 0.1\n").ui_scale, *UI_SCALES.start());
        assert_eq!(load_with("ui_scale = nan\n").ui_scale, 1.0);

        let dir = tempfile::tempdir().unwrap();
        let settings = Settings {
            ui_scale: 1.25,
            line_numbers: false,
            soft_wrap: true,
            row_cap: 0,
            ..Settings::default()
        };
        settings.save(dir.path()).unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), settings);
    }

    #[test]
    fn load_resets_a_nan_font_size() {
        let settings = load_with("mono_font_size = nan\n");
        assert_eq!(settings.mono_font_size, DEFAULT_MONO_FONT_SIZE);
    }

    /// Negative counts are clamped into range like any other out-of-range value.
    #[test]
    fn load_clamps_negative_counts() {
        let settings = load_with("page_size = -1\nconnect_timeout_s = -5\n");
        assert_eq!(settings.page_size, *PAGE_SIZES.start());
        assert_eq!(settings.connect_timeout_s, *CONNECT_TIMEOUTS_S.start());
    }
}
