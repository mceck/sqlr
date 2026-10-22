//! Finding Java and assembling the bridge's command line.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::MIN_JAVA;

#[derive(Debug, PartialEq, Eq)]
pub enum JavaError {
    NotFound,
    TooOld { found: u32, path: PathBuf },
}

impl std::fmt::Display for JavaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const HOW: &str = "Install a JRE or JDK (for example `brew install openjdk@21`, `apt install \
                           openjdk-21-jre`, or a build from https://adoptium.net) and restart sqlr, \
                           or point the JAVA_HOME environment variable at one.";
        match self {
            JavaError::NotFound => write!(
                f,
                "The JDBC bridge needs Java {MIN_JAVA} or newer, and none was found \
                 (looked at JAVA_HOME, the system Java and PATH). {HOW}"
            ),
            JavaError::TooOld { found, path } => write!(
                f,
                "The JDBC bridge needs Java {MIN_JAVA} or newer; {} is Java {found}. {HOW}",
                path.display()
            ),
        }
    }
}

fn java_name() -> &'static str {
    if cfg!(windows) { "java.exe" } else { "java" }
}

/// The `java` to run: `JAVA_HOME`, then macOS's registered JVMs, then `PATH`. `env` reads an
/// environment variable; `path` is the directories of `PATH`; `macos_homes` the JVM homes the
/// system lists. A candidate that is too old is skipped; if every one is, the newest of them
/// is reported.
pub fn find_java(
    env: &dyn Fn(&str) -> Option<OsString>,
    path: &[PathBuf],
    macos_homes: &[PathBuf],
    major_of: &dyn Fn(&Path) -> Option<u32>,
) -> Result<PathBuf, JavaError> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = env("JAVA_HOME") {
        candidates.push(PathBuf::from(home).join("bin").join(java_name()));
    }
    candidates.extend(macos_homes.iter().map(|h| h.join("bin").join(java_name())));
    candidates.extend(path.iter().map(|d| d.join(java_name())));
    let mut too_old = None;
    for candidate in candidates.into_iter().filter(|c| c.is_file()) {
        match major_of(&candidate) {
            Some(major) if major >= MIN_JAVA => return Ok(candidate),
            Some(found) => {
                too_old.get_or_insert(JavaError::TooOld {
                    found,
                    path: candidate,
                });
            }
            None => {}
        }
    }
    Err(too_old.unwrap_or(JavaError::NotFound))
}

/// The major version `java -version` reports (`17.0.2` → 17, `1.8.0_292` → 8).
pub fn java_major(java: &Path) -> Option<u32> {
    let output = Command::new(java).arg("-version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stderr);
    parse_major(&text)
}

fn parse_major(text: &str) -> Option<u32> {
    let version = text.split('"').nth(1)?;
    let mut parts = version.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// The system Java homes macOS knows (`/usr/libexec/java_home -V` lists them on stderr).
pub fn macos_java_homes() -> Vec<PathBuf> {
    // `SQLR_JDBC_NO_SYSTEM_JAVA` hides the system's Java (tests of the missing-JRE message).
    if !cfg!(target_os = "macos") || std::env::var_os("SQLR_JDBC_NO_SYSTEM_JAVA").is_some() {
        return Vec::new();
    }
    Command::new("/usr/libexec/java_home")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| vec![PathBuf::from(String::from_utf8_lossy(&o.stdout).trim())])
        .unwrap_or_default()
}

/// The bridge jar: next to the launcher, else the one built with it.
pub fn bridge_jar(exe_dir: &Path, built_with: &str) -> Option<PathBuf> {
    let beside = exe_dir.join("sqlr-jdbc-bridge.jar");
    if beside.is_file() {
        return Some(beside);
    }
    let built = PathBuf::from(built_with);
    (!built_with.is_empty() && built.is_file()).then_some(built)
}

/// The class path: the bridge jar, every jar in `jars_dir`, and `extra` (a platform
/// path-list, for tests and for jars already on the machine).
pub fn classpath(bridge: &Path, jars_dir: &Path, extra: Option<&str>) -> String {
    let sep = if cfg!(windows) { ";" } else { ":" };
    let mut entries = vec![bridge.display().to_string()];
    if let Ok(read) = std::fs::read_dir(jars_dir) {
        let mut jars: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jar"))
            .collect();
        jars.sort();
        entries.extend(jars.iter().map(|p| p.display().to_string()));
    }
    if let Some(extra) = extra.filter(|e| !e.is_empty()) {
        entries.push(extra.to_string());
    }
    entries.join(sep)
}

/// The command that runs the bridge for `preset_args`.
pub fn launch_command(java: &Path, classpath: &str, preset_args: &[String]) -> Command {
    let mut command = Command::new(java);
    command
        .arg("-Xshare:auto")
        .arg("-XX:+UseSerialGC")
        .arg("-cp")
        .arg(classpath)
        .arg("dev.sqlr.jdbc.Main")
        .args(preset_args);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    #[test]
    fn missing_jre_message() {
        let error = find_java(&no_env, &[], &[], &|_| None).unwrap_err();
        assert_eq!(error, JavaError::NotFound);
        let text = error.to_string();
        // It says what is needed, where it looked and how to get it.
        assert!(text.contains("Java 17"), "{text}");
        assert!(text.contains("JAVA_HOME"), "{text}");
        assert!(text.contains("adoptium"), "{text}");
    }

    #[test]
    fn a_too_old_java_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(java_name()), "").unwrap();
        let env = |k: &str| (k == "JAVA_HOME").then(|| dir.path().as_os_str().to_owned());
        let error = find_java(&env, &[], &[], &|_| Some(8)).unwrap_err();
        assert!(
            matches!(error, JavaError::TooOld { found: 8, .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("Java 8"));
        // The same file is fine when it is new enough.
        assert!(find_java(&env, &[], &[], &|_| Some(21)).is_ok());
    }

    #[test]
    fn java_home_wins_over_path() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        for d in [home.path(), other.path()] {
            std::fs::create_dir_all(d.join("bin")).unwrap();
            std::fs::write(d.join("bin").join(java_name()), "").unwrap();
        }
        let env = |k: &str| (k == "JAVA_HOME").then(|| home.path().as_os_str().to_owned());
        let found = find_java(&env, &[other.path().join("bin")], &[], &|_| Some(17)).unwrap();
        assert!(found.starts_with(home.path()));
    }

    #[test]
    fn version_banners_parse() {
        assert_eq!(
            parse_major("openjdk version \"21\" 2023-09-19\nOpenJDK Runtime"),
            Some(21)
        );
        assert_eq!(
            parse_major("openjdk version \"17.0.2\" 2022-01-18"),
            Some(17)
        );
        assert_eq!(parse_major("java version \"1.8.0_292\""), Some(8));
        assert_eq!(
            parse_major("openjdk version \"22-ea\" 2024-03-19"),
            Some(22)
        );
        assert_eq!(parse_major("not java"), None);
    }

    #[test]
    fn classpath_lists_bridge_then_jars_then_extra() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.jar"), "").unwrap();
        std::fs::write(dir.path().join("a.jar"), "").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        let cp = classpath(Path::new("/x/bridge.jar"), dir.path(), Some("/y/z.jar"));
        let parts: Vec<&str> = cp.split(if cfg!(windows) { ';' } else { ':' }).collect();
        assert_eq!(parts[0], "/x/bridge.jar");
        assert!(parts[1].ends_with("a.jar") && parts[2].ends_with("b.jar"));
        assert_eq!(parts[3], "/y/z.jar");
        assert_eq!(parts.len(), 4);
    }
}
