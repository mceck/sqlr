use std::path::{Path, PathBuf};

use sqlr_core::{ConnectionConfig, DriverManifest, Secrets};

use crate::{SecretStore, StoreError, write_atomic};

/// Where a moved item lands among its new siblings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoveTarget {
    Before(String),
    After(String),
    /// The end of the destination list.
    End,
    /// The end of the contents of the named folder.
    Into(String),
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct File {
    /// Layout revision: `0` is a file written before folders and explicit order existed.
    /// Scalars and arrays come before the array-of-tables so TOML keeps them at top level.
    #[serde(default)]
    version: u32,
    /// Folder paths in display order (`A/B` nests inside `A`). A connection's `group`
    /// names its folder.
    #[serde(default)]
    folder: Vec<String>,
    #[serde(default)]
    connection: Vec<ConnectionConfig>,
}

/// Connection configs and their folders in `<dir>/connections.toml`; secrets only in the
/// `SecretStore`.
pub struct ConnectionRepo {
    path: PathBuf,
    secrets: Box<dyn SecretStore>,
}

impl ConnectionRepo {
    pub fn open(dir: &Path, secrets: Box<dyn SecretStore>) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            path: dir.join("connections.toml"),
            secrets,
        })
    }

    fn read(&self) -> Result<File, StoreError> {
        let mut file: File = match std::fs::read_to_string(&self.path) {
            Ok(text) => toml::from_str(&text).map_err(|e| StoreError::Parse(e.to_string()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
            Err(e) => return Err(e.into()),
        };
        // A group is a folder path: trim its segments once, so the folder list and the
        // connections always agree on the spelling.
        for cfg in &mut file.connection {
            cfg.group = normalize_group(cfg.group.as_deref());
        }
        // Files from before drag-and-drop order were shown sorted; keep that order until the
        // first write records an explicit one.
        if file.version == 0 {
            file.connection
                .sort_by(|a, b| (&a.group, &a.name).cmp(&(&b.group, &b.name)));
        }
        Ok(file)
    }

    fn write(&self, mut file: File) -> Result<(), StoreError> {
        file.version = 1;
        let text = toml::to_string_pretty(&file).map_err(|e| StoreError::Parse(e.to_string()))?;
        write_atomic(&self.path, &text)
    }

    /// In file order (the order the user arranged).
    pub fn list(&self) -> Result<Vec<ConnectionConfig>, StoreError> {
        Ok(self.read()?.connection)
    }

    /// Folder paths in display order, including every ancestor and any folder named by a
    /// connection's `group` that the file does not list yet.
    pub fn folders(&self) -> Result<Vec<String>, StoreError> {
        Ok(folder_names(&self.read()?))
    }

    /// Upserts by id. Writes the TOML first, then each secret; an empty value deletes it.
    /// With `cfg.save_secrets` off, every secret named in `secrets` is deleted instead.
    /// `cfg.values` must not contain secret fields.
    pub fn save(&self, cfg: &ConnectionConfig, secrets: &Secrets) -> Result<(), StoreError> {
        let mut cfg = cfg.clone();
        cfg.group = normalize_group(cfg.group.as_deref());
        let mut file = self.read()?;
        materialize_folders(&mut file);
        match file.connection.iter_mut().find(|c| c.id == cfg.id) {
            Some(slot) => *slot = cfg.clone(),
            None => file.connection.push(cfg.clone()),
        }
        materialize_folders(&mut file);
        self.write(file)?;
        for (field, value) in &secrets.0 {
            if value.is_empty() || !cfg.save_secrets {
                self.secrets.delete(&cfg.id, field)?;
            } else {
                self.secrets.set(&cfg.id, field, value)?;
            }
        }
        Ok(())
    }

    /// Adds an empty folder at the end. Paths are `/`-separated, trimmed and unique; missing
    /// ancestors are added too, so nested folders can be created in one step. Returns the
    /// cleaned path.
    pub fn create_folder(&self, name: &str) -> Result<String, StoreError> {
        let name = clean_folder_name(name)?;
        let mut file = self.read()?;
        materialize_folders(&mut file);
        if file.folder.iter().any(|f| f == &name) {
            return Err(StoreError::Invalid(format!(
                "The folder \"{name}\" already exists"
            )));
        }
        file.folder.push(name.clone());
        self.write(file)?;
        Ok(name)
    }

    /// Renames a folder and every connection, subfolder and nested connection under it. The
    /// new path must keep the same parent: moving a folder is [`Self::move_folder`]'s job.
    /// Returns the cleaned path.
    pub fn rename_folder(&self, from: &str, to: &str) -> Result<String, StoreError> {
        let from = clean_folder_name(from)?;
        let to = clean_folder_name(to)?;
        if from == to {
            return Ok(to);
        }
        let mut file = self.read()?;
        materialize_folders(&mut file);
        if !file.folder.iter().any(|f| f == &from) {
            return Err(missing_folder(&from));
        }
        if parent_folder(&from) != parent_folder(&to) {
            return Err(StoreError::Invalid(
                "A folder cannot move while being renamed".into(),
            ));
        }
        let renamed = rewrite_paths(&file.folder, &from, &to);
        if has_duplicates(&renamed) {
            return Err(StoreError::Invalid(format!(
                "The folder \"{to}\" already exists"
            )));
        }
        file.folder = renamed;
        for cfg in &mut file.connection {
            if let Some(group) = cfg.group.as_deref() {
                cfg.group = Some(rewrite_path(group, &from, &to));
            }
        }
        self.write(file)?;
        Ok(to)
    }

    /// Removes the folder; its connections and subfolders move to its parent (the top level
    /// for a top-level folder). No connection is deleted.
    pub fn delete_folder(&self, name: &str) -> Result<(), StoreError> {
        let name = clean_folder_name(name)?;
        let mut file = self.read()?;
        materialize_folders(&mut file);
        let parent = parent_folder(&name).map(str::to_string);
        let prefix = format!("{name}/");
        let mut kept = Vec::with_capacity(file.folder.len());
        for folder in &file.folder {
            if folder == &name {
                continue;
            }
            let folder = match folder.strip_prefix(&prefix) {
                Some(rest) => join_folder(parent.as_deref(), rest),
                None => folder.clone(),
            };
            if !kept.contains(&folder) {
                kept.push(folder);
            }
        }
        file.folder = kept;
        for cfg in &mut file.connection {
            if let Some(group) = cfg.group.as_deref() {
                match group.strip_prefix(&prefix) {
                    Some(rest) => cfg.group = Some(join_folder(parent.as_deref(), rest)),
                    None if group == name => cfg.group = parent.clone(),
                    None => {}
                }
            }
        }
        self.write(file)
    }

    /// Moves a connection to `folder` (`None` is the top level), landing before or after
    /// another connection of that folder or at its end.
    pub fn move_connection(
        &self,
        conn: &str,
        folder: Option<&str>,
        target: MoveTarget,
    ) -> Result<(), StoreError> {
        let folder = match folder {
            Some(name) => Some(clean_folder_name(name)?),
            None => None,
        };
        let mut file = self.read()?;
        materialize_folders(&mut file);
        if let Some(name) = &folder
            && !file.folder.iter().any(|f| f == name)
        {
            return Err(missing_folder(name));
        }
        let Some(pos) = file.connection.iter().position(|c| c.id == conn) else {
            return Err(StoreError::Invalid(format!(
                "The connection \"{conn}\" does not exist"
            )));
        };
        let mut cfg = file.connection.remove(pos);
        cfg.group = folder;
        let insert = match target {
            MoveTarget::Before(id) => {
                insert_position(&file.connection, cfg.group.as_deref(), &id, true)
            }
            MoveTarget::After(id) => {
                insert_position(&file.connection, cfg.group.as_deref(), &id, false)
            }
            MoveTarget::End | MoveTarget::Into(_) => {
                end_of_folder(&file.connection, cfg.group.as_deref())
            }
        };
        file.connection.insert(insert, cfg);
        self.write(file)
    }

    /// Moves a folder (with its subfolders and connections) before or after another folder,
    /// into a folder, or to the end of the top level. Returns its new path.
    pub fn move_folder(&self, name: &str, target: MoveTarget) -> Result<String, StoreError> {
        let name = clean_folder_name(name)?;
        let mut file = self.read()?;
        materialize_folders(&mut file);
        if !file.folder.iter().any(|f| f == &name) {
            return Err(missing_folder(&name));
        }
        let prefix = format!("{name}/");
        let leaf = name.rsplit('/').next().unwrap_or(&name);
        let parent = match &target {
            MoveTarget::Before(other) | MoveTarget::After(other) => {
                let other = clean_folder_name(other)?;
                if other == name || other.starts_with(&prefix) {
                    return Err(StoreError::Invalid(
                        "A folder cannot move next to itself".into(),
                    ));
                }
                if !file.folder.iter().any(|f| f == &other) {
                    return Err(missing_folder(&other));
                }
                parent_folder(&other).map(str::to_string)
            }
            MoveTarget::End => None,
            MoveTarget::Into(dest) => {
                let dest = clean_folder_name(dest)?;
                if dest == name || dest.starts_with(&prefix) {
                    return Err(StoreError::Invalid(
                        "A folder cannot move inside itself".into(),
                    ));
                }
                if !file.folder.iter().any(|f| f == &dest) {
                    return Err(missing_folder(&dest));
                }
                Some(dest)
            }
        };
        let new_name = join_folder(parent.as_deref(), leaf);
        if new_name != name && file.folder.contains(&new_name) {
            return Err(StoreError::Invalid(format!(
                "The folder \"{new_name}\" already exists"
            )));
        }
        let mut rest = Vec::with_capacity(file.folder.len());
        let mut subtree = Vec::new();
        for folder in file.folder.drain(..) {
            if folder == name {
                subtree.push(new_name.clone());
            } else if let Some(tail) = folder.strip_prefix(&prefix) {
                subtree.push(format!("{new_name}/{tail}"));
            } else {
                rest.push(folder);
            }
        }
        let at = match &target {
            MoveTarget::Before(other) => {
                let other = clean_folder_name(other)?;
                rest.iter()
                    .position(|f| *f == other)
                    .ok_or_else(|| missing_folder(&other))?
            }
            MoveTarget::After(other) => {
                let other = clean_folder_name(other)?;
                rest.iter()
                    .position(|f| *f == other)
                    .map(|i| i + 1)
                    .ok_or_else(|| missing_folder(&other))?
            }
            MoveTarget::End => rest.len(),
            MoveTarget::Into(dest) => {
                let dest = clean_folder_name(dest)?;
                let dest_prefix = format!("{dest}/");
                rest.iter()
                    .rposition(|f| *f == dest || f.starts_with(&dest_prefix))
                    .map(|i| i + 1)
                    .ok_or_else(|| missing_folder(&dest))?
            }
        };
        for (offset, folder) in subtree.into_iter().enumerate() {
            rest.insert(at + offset, folder);
        }
        file.folder = rest;
        for cfg in &mut file.connection {
            if let Some(group) = cfg.group.as_deref() {
                cfg.group = Some(rewrite_path(group, &name, &new_name));
            }
        }
        self.write(file)?;
        Ok(new_name)
    }

    /// Removes the config and its secret fields.
    pub fn delete(&self, id: &str, manifest: &DriverManifest) -> Result<(), StoreError> {
        let mut file = self.read()?;
        file.connection.retain(|c| c.id != id);
        materialize_folders(&mut file);
        self.write(file)?;
        for f in manifest.fields.iter().filter(|f| f.secret) {
            self.secrets.delete(id, &f.key)?;
        }
        Ok(())
    }

    /// Stored values of the manifest's secret fields.
    pub fn secrets_for(
        &self,
        cfg: &ConnectionConfig,
        manifest: &DriverManifest,
    ) -> Result<Secrets, StoreError> {
        let mut out = Secrets::default();
        for f in manifest.fields.iter().filter(|f| f.secret) {
            if let Some(v) = self.secrets.get(&cfg.id, &f.key)? {
                out.0.insert(f.key.clone(), v);
            }
        }
        Ok(out)
    }
}

/// A folder path with trimmed, non-empty `/`-separated segments.
fn clean_folder_name(name: &str) -> Result<String, StoreError> {
    let segments: Vec<&str> = name
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() {
        Err(StoreError::Invalid("A folder needs a name".into()))
    } else {
        Ok(segments.join("/"))
    }
}

/// A connection's `group` as a folder path; blank groups mean the top level.
fn normalize_group(group: Option<&str>) -> Option<String> {
    group.and_then(|g| clean_folder_name(g).ok())
}

/// The folder that contains `path`, or the top level.
fn parent_folder(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

/// `parent/name`, or `name` at the top level.
fn join_folder(parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(parent) if !parent.is_empty() => format!("{parent}/{name}"),
        _ => name.to_string(),
    }
}

/// `path` with the `from` prefix replaced by `to`; other paths are unchanged.
fn rewrite_path(path: &str, from: &str, to: &str) -> String {
    if path == from {
        return to.to_string();
    }
    match path.strip_prefix(&format!("{from}/")) {
        Some(tail) => format!("{to}/{tail}"),
        None => path.to_string(),
    }
}

/// `paths` with their `from` prefix replaced by `to`, keeping their order.
fn rewrite_paths(paths: &[String], from: &str, to: &str) -> Vec<String> {
    paths.iter().map(|p| rewrite_path(p, from, to)).collect()
}

fn has_duplicates(paths: &[String]) -> bool {
    let mut seen = std::collections::HashSet::with_capacity(paths.len());
    paths.iter().any(|p| !seen.insert(p.as_str()))
}

fn missing_folder(name: &str) -> StoreError {
    StoreError::Invalid(format!("The folder \"{name}\" does not exist"))
}

/// Explicit folders first, then groups only connections name, sorted by name. Every path is
/// normalised and its ancestors are listed before it.
fn folder_names(file: &File) -> Vec<String> {
    let mut out = Vec::new();
    for name in &file.folder {
        add_folder_path(&mut out, name);
    }
    let mut implicit: Vec<&str> = file
        .connection
        .iter()
        .filter_map(|c| c.group.as_deref())
        .filter(|g| !g.trim().is_empty())
        .collect();
    implicit.sort_unstable();
    for name in implicit {
        add_folder_path(&mut out, name);
    }
    out
}

/// Adds `name` and each missing ancestor, keeping the first appearance's order.
fn add_folder_path(out: &mut Vec<String>, name: &str) {
    let Ok(name) = clean_folder_name(name) else {
        return;
    };
    let mut prefix = String::new();
    for segment in name.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        if !out.contains(&prefix) {
            out.push(prefix.clone());
        }
    }
}

/// Writes every group a connection names into the folder list, preserving existing order.
fn materialize_folders(file: &mut File) {
    file.folder = folder_names(file);
}

/// The index just after the last connection of `folder` in `conns`.
fn end_of_folder(conns: &[ConnectionConfig], folder: Option<&str>) -> usize {
    conns
        .iter()
        .rposition(|c| c.group.as_deref() == folder)
        .map_or(conns.len(), |i| i + 1)
}

/// Where `id`'s connection sits in `folder`, when it belongs to that folder.
fn insert_position(
    conns: &[ConnectionConfig],
    folder: Option<&str>,
    id: &str,
    before: bool,
) -> usize {
    match conns
        .iter()
        .position(|c| c.id == id && c.group.as_deref() == folder)
    {
        Some(i) if before => i,
        Some(i) => i + 1,
        None => end_of_folder(conns, folder),
    }
}
