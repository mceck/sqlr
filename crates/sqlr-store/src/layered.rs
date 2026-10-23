//! Layered settings (§4.3): the global `settings.toml`, and the overrides a connection
//! profile or one connection sets in `overrides.toml`. A value resolves connection →
//! profile → global → default.
//!
//! `settings.toml` written by earlier versions is flat (`page_size = 500`); it loads into the
//! model with every value kept and is backed up once as `settings.toml.bak-m9` before the
//! first write in the new shape (`version = 2` and a `[values]` table).

use std::collections::BTreeMap;
use std::path::Path;

use sqlr_core::{Level, Levels, Safety, SettingDef, SettingValue, search_settings, setting_def};

use crate::{StoreError, write_atomic};

const VERSION: i64 = 2;
const SETTINGS_FILE: &str = "settings.toml";
const OVERRIDES_FILE: &str = "overrides.toml";

type Values = BTreeMap<String, SettingValue>;

/// The settings at every level.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayeredSettings {
    global: Values,
    profiles: BTreeMap<String, Values>,
    connections: BTreeMap<String, Values>,
}

/// What loading found besides the values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadNotice {
    /// A file that could not be read: it was kept as `<name>.corrupt` and the defaults are
    /// in force. The app shows this as a toast.
    pub corrupt: Option<String>,
}

impl LayeredSettings {
    /// Reads `settings.toml` (either shape) and `overrides.toml`. A missing file is empty; a
    /// file that does not parse is copied to `.corrupt` and replaced by the defaults.
    pub fn load(dir: &Path) -> (Self, LoadNotice) {
        let mut notice = LoadNotice::default();
        let mut me = Self::default();
        match read_toml(&dir.join(SETTINGS_FILE)) {
            Ok(Some(table)) => me.global = read_global(&table),
            Ok(None) => {}
            Err(e) => notice.corrupt = Some(keep_corrupt(dir, SETTINGS_FILE, e)),
        }
        match read_toml(&dir.join(OVERRIDES_FILE)) {
            Ok(Some(table)) => {
                me.profiles = read_overrides(&table, "profile");
                me.connections = read_overrides(&table, "connection");
            }
            Ok(None) => {}
            Err(e) => {
                let message = keep_corrupt(dir, OVERRIDES_FILE, e);
                notice.corrupt = Some(match notice.corrupt {
                    Some(first) => format!("{first}; {message}"),
                    None => message,
                });
            }
        }
        (me, notice)
    }

    /// Writes both files. An old flat `settings.toml` is backed up first, once.
    pub fn save(&self, dir: &Path) -> Result<(), StoreError> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(SETTINGS_FILE);
        let backup = dir.join("settings.toml.bak-m9");
        if !backup.exists()
            && let Ok(Some(old)) = read_toml(&path)
            && !old.contains_key("version")
        {
            std::fs::copy(&path, &backup)?;
        }
        let mut file = toml::Table::new();
        file.insert("version".into(), toml::Value::Integer(VERSION));
        file.insert("values".into(), to_toml_table(&self.global));
        write_atomic(&path, &to_text(&file)?)?;

        let mut overrides = toml::Table::new();
        for (key, groups) in [
            ("profile", &self.profiles),
            ("connection", &self.connections),
        ] {
            let mut table = toml::Table::new();
            for (id, values) in groups.iter().filter(|(_, v)| !v.is_empty()) {
                table.insert(id.clone(), to_toml_table(values));
            }
            if !table.is_empty() {
                overrides.insert(key.into(), toml::Value::Table(table));
            }
        }
        let overrides_path = dir.join(OVERRIDES_FILE);
        if overrides.is_empty() {
            match std::fs::remove_file(&overrides_path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        } else {
            write_atomic(&overrides_path, &to_text(&overrides)?)?;
        }
        Ok(())
    }

    fn layer(&self, level: &Level) -> Option<&Values> {
        match level {
            Level::Global => Some(&self.global),
            Level::Profile(id) => self.profiles.get(id),
            Level::Connection(id) => self.connections.get(id),
        }
    }

    /// The value set at exactly this level, if any.
    pub fn value_at(&self, id: &str, level: &Level) -> Option<&SettingValue> {
        self.layer(level)?.get(id)
    }

    /// Every stored value with its level key (`global`, `profile:<id>`,
    /// `connection:<id>`), for the read-only settings view agents get.
    pub fn stored_values(&self) -> Vec<(String, String, SettingValue)> {
        let mut out = Vec::new();
        for (id, value) in &self.global {
            out.push(("global".to_string(), id.clone(), value.clone()));
        }
        for (profile, values) in &self.profiles {
            for (id, value) in values {
                out.push((format!("profile:{profile}"), id.clone(), value.clone()));
            }
        }
        for (connection, values) in &self.connections {
            for (id, value) in values {
                out.push((
                    format!("connection:{connection}"),
                    id.clone(),
                    value.clone(),
                ));
            }
        }
        out
    }

    /// The value in force for `id` and where it comes from. `conn` is the connection's id and
    /// its profile's id; without it only the global level applies. A safety flag is on when
    /// any level has it on.
    pub fn get(&self, id: &str, conn: Option<(&str, &str)>) -> (SettingValue, Level) {
        let Some(def) = setting_def(id) else {
            debug_assert!(false, "unknown setting {id}");
            return (SettingValue::Bool(false), Level::Global);
        };
        let mut chain = Vec::with_capacity(3);
        if def.levels == Levels::All
            && let Some((conn_id, profile_id)) = conn
        {
            chain.push(Level::Connection(conn_id.to_string()));
            chain.push(Level::Profile(profile_id.to_string()));
        }
        chain.push(Level::Global);
        if def.safety == Safety::TightenOnly {
            let on = chain
                .into_iter()
                .find(|l| self.value_at(id, l).is_some_and(SettingValue::as_bool));
            return match on {
                Some(level) => (SettingValue::Bool(true), level),
                None => (def.default.clone(), Level::Global),
            };
        }
        for level in chain {
            if let Some(v) = self.value_at(id, &level) {
                return (v.clone(), level);
            }
        }
        (def.default.clone(), Level::Global)
    }

    /// Sets (or, with `None`, clears) a value at a level. A value outside the setting's
    /// type or range, a level the setting does not allow, and loosening a safety flag below
    /// the global level are refused.
    pub fn set(
        &mut self,
        id: &str,
        level: Level,
        value: Option<SettingValue>,
    ) -> Result<(), String> {
        let def: &SettingDef = setting_def(id).ok_or_else(|| format!("unknown setting {id}"))?;
        if level != Level::Global && def.levels == Levels::GlobalOnly {
            return Err(format!("{} can only be set globally", def.label));
        }
        if let Some(v) = &value {
            def.check(v)?;
            if def.safety == Safety::TightenOnly && level != Level::Global && !v.as_bool() {
                return Err(format!(
                    "{} can only be switched on for a profile or a connection; clear the override instead",
                    def.label
                ));
            }
        }
        let layer = match &level {
            Level::Global => &mut self.global,
            Level::Profile(p) => self.profiles.entry(p.clone()).or_default(),
            Level::Connection(c) => self.connections.entry(c.clone()).or_default(),
        };
        match value {
            Some(v) => {
                layer.insert(id.to_string(), v);
            }
            None => {
                layer.remove(id);
            }
        }
        self.profiles.retain(|_, v| !v.is_empty());
        self.connections.retain(|_, v| !v.is_empty());
        Ok(())
    }

    /// Forgets every override of a deleted profile or connection.
    pub fn forget(&mut self, level: &Level) {
        match level {
            Level::Global => self.global.clear(),
            Level::Profile(id) => {
                self.profiles.remove(id);
            }
            Level::Connection(id) => {
                self.connections.remove(id);
            }
        }
    }

    /// The settings matching a search query; see [`search_settings`].
    pub fn search(&self, q: &str) -> Vec<&'static SettingDef> {
        search_settings(q)
    }
}

fn read_toml(path: &Path) -> Result<Option<toml::Table>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<toml::Table>()
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Copies an unreadable file aside and says so.
pub(crate) fn keep_corrupt(dir: &Path, name: &str, error: String) -> String {
    let from = dir.join(name);
    let to = dir.join(format!("{name}.corrupt"));
    let kept = std::fs::copy(&from, &to).is_ok();
    format!(
        "{name} could not be read ({error}); the defaults are in use{}",
        if kept {
            format!(" and the file was kept as {name}.corrupt")
        } else {
            String::new()
        }
    )
}

/// The global values of either file shape.
fn read_global(table: &toml::Table) -> Values {
    let mut out = Values::new();
    if table.contains_key("version") {
        if let Some(toml::Value::Table(values)) = table.get("values") {
            read_values(values, &mut out);
        }
        return out;
    }
    // The earlier flat settings file: each key moved to its dotted id.
    const OLD: &[(&str, &str)] = &[
        ("page_size", "grid.page_size"),
        ("row_cap", "grid.row_cap"),
        ("stop_script_on_error", "general.stop_script_on_error"),
        ("theme", "appearance.theme"),
        ("mono_font_size", "editor.font_size"),
        ("session_per_tab", "connection.session_per_tab"),
        ("connect_timeout_s", "connection.connect_timeout_s"),
        ("ui_scale", "appearance.ui_scale"),
        ("line_numbers", "editor.line_numbers"),
        ("soft_wrap", "editor.soft_wrap"),
        ("sidecar_idle_minutes", "drivers.sidecar_idle_minutes"),
        ("agents_enabled", "agents.enabled"),
        ("agents_port", "agents.port"),
        ("terminal_shell", "terminal.shell"),
        ("terminal_cwd", "terminal.cwd"),
        ("terminal_scrollback", "terminal.scrollback"),
    ];
    for (old, new) in OLD {
        let Some(value) = table.get(*old) else {
            continue;
        };
        let Some(def) = setting_def(new) else {
            continue;
        };
        let mut json = to_json(value);
        // `ThemePref` was written capitalized (`Light`).
        if *old == "theme"
            && let Some(s) = json.as_str()
        {
            json = s.to_lowercase().into();
        }
        if let Some(v) = def.from_json(&json) {
            out.insert((*new).to_string(), v);
        }
    }
    out
}

fn read_overrides(table: &toml::Table, key: &str) -> BTreeMap<String, Values> {
    let mut out = BTreeMap::new();
    if let Some(toml::Value::Table(groups)) = table.get(key) {
        for (id, values) in groups {
            if let toml::Value::Table(values) = values {
                let mut layer = Values::new();
                read_values(values, &mut layer);
                // A tampered file must not loosen a safety flag or set a global-only key.
                layer.retain(|k, v| {
                    setting_def(k).is_some_and(|d| {
                        d.levels == Levels::All && (d.safety != Safety::TightenOnly || v.as_bool())
                    })
                });
                out.insert(id.clone(), layer);
            }
        }
    }
    out
}

/// Known keys with the right shape, clamped into range; anything else is dropped.
fn read_values(values: &toml::Table, out: &mut Values) {
    for (id, value) in values {
        if let Some(def) = setting_def(id)
            && let Some(v) = def.from_json(&to_json(value))
        {
            out.insert(id.clone(), v);
        }
    }
}

fn to_json(value: &toml::Value) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

fn to_toml_table(values: &Values) -> toml::Value {
    let mut table = toml::Table::new();
    for (id, v) in values {
        if let Ok(t) = toml::Value::try_from(v.to_json()) {
            table.insert(id.clone(), t);
        }
    }
    toml::Value::Table(table)
}

fn to_text(table: &toml::Table) -> Result<String, StoreError> {
    toml::to_string_pretty(table).map_err(|e| StoreError::Parse(e.to_string()))
}
