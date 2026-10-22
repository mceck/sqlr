//! The keymap in `<dir>/keymap.toml`: the preset's name and what differs from it.

use std::path::Path;

use sqlr_core::Keymap;

use crate::layered::keep_corrupt;
use crate::{StoreError, write_atomic};

const FILE: &str = "keymap.toml";

/// A loaded keymap: the bindings, the preset they started from and, when the file was
/// unreadable, what the user should be told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredKeymap {
    pub keymap: Keymap,
    pub preset: String,
    pub corrupt: Option<String>,
}

impl StoredKeymap {
    fn default_keymap() -> Self {
        Self {
            keymap: Keymap::preset("sqlr").unwrap_or_default(),
            preset: "sqlr".into(),
            corrupt: None,
        }
    }

    fn corrupt(dir: &Path, error: String) -> Self {
        Self {
            corrupt: Some(keep_corrupt(dir, FILE, error)),
            ..Self::default_keymap()
        }
    }

    /// A missing file is the `sqlr` preset; an unreadable one is kept as `.corrupt` and
    /// replaced by it.
    pub fn load(dir: &Path) -> Self {
        let text = match std::fs::read_to_string(dir.join(FILE)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default_keymap(),
            Err(e) => return Self::corrupt(dir, e.to_string()),
        };
        let table = match text.parse::<toml::Table>() {
            Ok(table) => table,
            Err(e) => return Self::corrupt(dir, e.to_string()),
        };
        let preset = table
            .get("preset")
            .and_then(|p| p.as_str())
            .unwrap_or("sqlr");
        let parts = table
            .get("bindings")
            .and_then(|b| b.as_table())
            .into_iter()
            .flatten()
            .filter_map(|(id, chords)| {
                let list = chords.as_array()?;
                let chords = list
                    .iter()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect();
                Some((id.clone(), chords))
            });
        let (keymap, preset) = Keymap::from_parts(preset, parts);
        Self {
            keymap,
            preset,
            corrupt: None,
        }
    }

    /// Writes the preset's name and the bindings that differ from it.
    pub fn save(&self, dir: &Path) -> Result<(), StoreError> {
        std::fs::create_dir_all(dir)?;
        let mut table = toml::Table::new();
        table.insert("preset".into(), toml::Value::String(self.preset.clone()));
        let mut bindings = toml::Table::new();
        for (id, chords) in self.keymap.to_parts(&self.preset) {
            let list = chords.into_iter().map(toml::Value::String).collect();
            bindings.insert(id.to_string(), toml::Value::Array(list));
        }
        if !bindings.is_empty() {
            table.insert("bindings".into(), toml::Value::Table(bindings));
        }
        let text = toml::to_string_pretty(&table).map_err(|e| StoreError::Parse(e.to_string()))?;
        write_atomic(&dir.join(FILE), &text)
    }
}
