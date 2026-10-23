use std::io::Read;
use std::path::{Path, PathBuf};

use super::{ClientFormat, ImportBatch, decode, detect};

const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct DetectedSource {
    pub format: ClientFormat,
    pub path: PathBuf,
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|_| {
        "Could not open the connection file. Check its location and permissions.".to_string()
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the connection file.".to_string())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("Connection file exceeds the 16 MiB limit.".into());
    }
    Ok(bytes)
}

pub fn read_source(path: &Path) -> Result<(ClientFormat, ImportBatch), String> {
    let bytes = read(path)?;
    let format = detect(&bytes)?;
    let batch = if format == ClientFormat::Datagrip {
        let sibling = path.with_file_name("dataSources.local.xml");
        let local = if sibling != path && sibling.is_file() {
            Some(read(&sibling)?)
        } else {
            None
        };
        super::import::datagrip_file(&bytes, local.as_deref())?
    } else {
        decode(format, &bytes)?
    };
    Ok((format, batch))
}

fn children(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .take(64)
        .collect();
    paths.sort();
    paths
}

/// Bounded search in known client folders only, never a recursive scan of the home.
/// `home` is supplied explicitly so discovery is deterministic in tests.
pub fn discover(home: &Path) -> Vec<DetectedSource> {
    let mut found = Vec::new();
    let mut add = |format, path: PathBuf| {
        if path.is_file() {
            found.push(DetectedSource { format, path });
        }
    };
    let appdata = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Roaming"));
    for root in [
        home.join("Library/DBeaverData"),
        home.join(".local/share/DBeaverData"),
        appdata.join("DBeaverData"),
    ] {
        for project in children(&root.join("workspace6")) {
            add(
                ClientFormat::Dbeaver,
                project.join(".dbeaver/data-sources.json"),
            );
        }
    }
    for app in ["com.tinyapp.TablePlus", "com.tinyapp.TablePlus-setapp"] {
        add(
            ClientFormat::TablePlus,
            home.join("Library/Application Support")
                .join(app)
                .join("Data/Connections.plist"),
        );
    }
    for root in [
        home.join("Library/Application Support/JetBrains"),
        home.join(".config/JetBrains"),
        appdata.join("JetBrains"),
    ] {
        for version in children(&root).into_iter().filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("DataGrip"))
        }) {
            add(
                ClientFormat::Datagrip,
                version.join("options/dataSources.xml"),
            );
            for project in children(&version.join("projects")) {
                add(
                    ClientFormat::Datagrip,
                    project.join(".idea/dataSources.xml"),
                );
            }
        }
    }
    for root in [
        home.join("DataGripProjects"),
        home.join("Documents/DataGripProjects"),
    ] {
        for project in children(&root) {
            add(
                ClientFormat::Datagrip,
                project.join(".idea/dataSources.xml"),
            );
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found.dedup_by(|a, b| a.path == b.path);
    found
}
