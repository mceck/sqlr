//! Connection profiles in `<dir>/profiles.toml`: user profiles and overrides of the
//! built-ins (the built-ins themselves are never written).

use std::path::{Path, PathBuf};

use sqlr_core::ConnectionProfile;

use crate::{StoreError, write_atomic};

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct File {
    #[serde(default)]
    profile: Vec<ConnectionProfile>,
}

pub struct ProfileRepo {
    path: PathBuf,
}

impl ProfileRepo {
    /// Nothing is read or created until the first call.
    pub fn open(dir: &Path) -> Self {
        Self {
            path: dir.join("profiles.toml"),
        }
    }

    fn read(&self) -> Result<File, StoreError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => toml::from_str(&text).map_err(|e| StoreError::Parse(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&self, file: &File) -> Result<(), StoreError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(file).map_err(|e| StoreError::Parse(e.to_string()))?;
        write_atomic(&self.path, &text)
    }

    /// The built-ins (each replaced by its override, if any), then the user profiles by name.
    pub fn list(&self) -> Result<Vec<ConnectionProfile>, StoreError> {
        let mut stored = self.read()?.profile;
        let mut out: Vec<ConnectionProfile> = ConnectionProfile::builtins()
            .into_iter()
            .map(|b| match stored.iter().position(|p| p.id == b.id) {
                Some(i) => stored.remove(i),
                None => b,
            })
            .collect();
        stored.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        out.extend(stored);
        Ok(out)
    }

    /// Upserts by id; for a built-in id this stores an override.
    pub fn save(&self, p: &ConnectionProfile) -> Result<(), StoreError> {
        let mut file = self.read()?;
        match file.profile.iter_mut().find(|s| s.id == p.id) {
            Some(slot) => *slot = p.clone(),
            None => file.profile.push(p.clone()),
        }
        self.write(&file)
    }

    /// Removes a user profile; for a built-in id, removes its override (a no-op without one).
    pub fn delete(&self, id: &str) -> Result<(), StoreError> {
        let mut file = self.read()?;
        let before = file.profile.len();
        file.profile.retain(|p| p.id != id);
        if file.profile.len() == before {
            return Ok(());
        }
        self.write(&file)
    }
}
