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
                 (looked at JAVA_HOME, the system Java, the usual install folders and PATH). {HOW}"
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

/// The `java` to run: `JAVA_HOME`, then the system's and the usual installers' Java homes
/// ([`system_java_homes`]), then `PATH`. `env` reads an environment variable; `path` is the
/// directories of `PATH`; `macos_homes` the Java homes to look in. A candidate that is too old is skipped; if every one is, the newest of them
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
    // The JVM may print `Picked up JAVA_TOOL_OPTIONS: "…"` (or `_JAVA_OPTIONS`)
    // before the banner, so look for the quoted string after `version`, not merely
    // the first quoted string.
    let version = text.split("version \"").nth(1)?.split('"').next()?;
    let mut parts = version.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// The Java homes the system and the usual installers know, best first: on macOS the one
/// `/usr/libexec/java_home` reports, then [`well_known_java_homes`]. An app started from the
/// Finder or a desktop launcher inherits no `JAVA_HOME` and a bare `PATH`, so a Java from a
/// version manager (asdf, SDKMAN, mise…), Homebrew or an IDE is found only here.
pub fn system_java_homes() -> Vec<PathBuf> {
    // `SQLR_JDBC_NO_SYSTEM_JAVA` hides the system's Java (tests of the missing-JRE message).
    if std::env::var_os("SQLR_JDBC_NO_SYSTEM_JAVA").is_some() {
        return Vec::new();
    }
    let mut homes = Vec::new();
    if cfg!(target_os = "macos") {
        homes.extend(
            Command::new("/usr/libexec/java_home")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim())),
        );
    }
    let user = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    homes.extend(well_known_java_homes(user.as_deref(), &system_roots()));
    homes
}

/// The directories, outside the user's home, where installers put one Java per child.
fn system_roots() -> Vec<PathBuf> {
    let roots: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/Library/Java/JavaVirtualMachines",
            "/opt/homebrew/opt",
            "/usr/local/opt",
        ]
    } else if cfg!(windows) {
        &[
            "C:\\Program Files\\Java",
            "C:\\Program Files\\Eclipse Adoptium",
            "C:\\Program Files\\Microsoft",
            "C:\\Program Files\\Zulu",
            "C:\\Program Files\\Amazon Corretto",
        ]
    } else {
        &["/usr/lib/jvm", "/usr/java", "/opt/java", "/opt"]
    };
    roots.iter().map(PathBuf::from).collect()
}

/// The Java homes under the usual install roots: the version managers' and IDEs' folders in
/// `user_home`, then `system_roots`. Each child of a root is one Java (a macOS bundle's home
/// is its `Contents/Home`); newer versions come first. Only folders that hold a `bin/java`
/// are listed.
pub fn well_known_java_homes(user_home: Option<&Path>, system_roots: &[PathBuf]) -> Vec<PathBuf> {
    const USER_ROOTS: [&str; 8] = [
        ".asdf/installs/java",
        ".sdkman/candidates/java",
        ".local/share/mise/installs/java",
        ".jenv/versions",
        "Library/Java/JavaVirtualMachines",
        ".jdks",
        ".gradle/jdks",
        ".local/share/rtx/installs/java",
    ];
    let mut roots: Vec<PathBuf> = user_home
        .map(|home| USER_ROOTS.iter().map(|r| home.join(r)).collect())
        .unwrap_or_default();
    roots.extend(system_roots.iter().cloned());
    let mut homes = Vec::new();
    for root in roots {
        let Ok(read) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut children: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
        // Newest first: by the first number in the folder name, then by name.
        children.sort_by(|a, b| {
            let key = |p: &PathBuf| {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
                let name = name.unwrap_or_default();
                let digits: String = name
                    .chars()
                    .skip_while(|c| !c.is_ascii_digit())
                    .take_while(char::is_ascii_digit)
                    .collect();
                (digits.parse::<u32>().unwrap_or(0), name)
            };
            key(b).cmp(&key(a))
        });
        for child in children {
            let bundle = child.join("Contents").join("Home");
            let home = if bundle.is_dir() { bundle } else { child };
            if home.join("bin").join(java_name()).is_file() && !homes.contains(&home) {
                homes.push(home);
            }
        }
    }
    homes
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
    fn version_managers_and_bundles_are_found_newest_first() {
        let user = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        let java = |home: &Path| {
            std::fs::create_dir_all(home.join("bin")).unwrap();
            std::fs::write(home.join("bin").join(java_name()), "").unwrap();
        };
        let asdf = user.path().join(".asdf/installs/java");
        java(&asdf.join("openjdk-17"));
        java(&asdf.join("openjdk-21"));
        // An IDE-downloaded JDK is a macOS bundle.
        let ide = user
            .path()
            .join("Library/Java/JavaVirtualMachines/corretto-22.0.1");
        java(&ide.join("Contents/Home"));
        // A folder without a java is not a home.
        std::fs::create_dir_all(system.path().join("not-java")).unwrap();
        java(&system.path().join("temurin-11"));
        let homes = well_known_java_homes(Some(user.path()), &[system.path().to_path_buf()]);
        assert_eq!(
            homes,
            vec![
                asdf.join("openjdk-21"),
                asdf.join("openjdk-17"),
                ide.join("Contents/Home"),
                system.path().join("temurin-11"),
            ]
        );
        // The launcher then picks the first one that is new enough.
        let javas: Vec<PathBuf> = homes;
        let found = find_java(&no_env, &[], &javas, &|_| Some(21)).unwrap();
        assert_eq!(found, asdf.join("openjdk-21/bin").join(java_name()));
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
    fn version_banners_ignore_picked_up_options() {
        // `JAVA_TOOL_OPTIONS` and `_JAVA_OPTIONS` make the JVM print a banner before the
        // version, so the first quoted string is an option rather than the version.
        assert_eq!(
            parse_major(
                "Picked up JAVA_TOOL_OPTIONS: \"-Dfile.encoding=UTF-8\"\n\
                 openjdk version \"21.0.1\" 2023-10-17"
            ),
            Some(21)
        );
        assert_eq!(
            parse_major("Picked up _JAVA_OPTIONS: \"-Xmx512m\"\njava version \"1.8.0_292\""),
            Some(8)
        );
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
