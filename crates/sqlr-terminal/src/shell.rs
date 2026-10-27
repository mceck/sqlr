//! How the configured shell becomes a command: a login shell by default, a command string
//! through the platform shell, and a working directory.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// The platform's automatic shell: the user's `$SHELL` with login behaviour on Unix, or
/// PowerShell when present, then `cmd.exe`, on Windows.
pub fn resolve(
    shell: &str,
    cwd: &str,
    home: Option<PathBuf>,
    unix_shell: Option<String>,
    mut _find: impl FnMut(&str) -> bool,
) -> CommandSpec {
    let cwd = if cwd.trim().is_empty() {
        home.unwrap_or_else(|| PathBuf::from("."))
    } else {
        PathBuf::from(cwd)
    };
    let shell = shell.trim();
    #[cfg(windows)]
    {
        if shell.is_empty() {
            let program = if _find("pwsh.exe") {
                "pwsh.exe"
            } else if _find("powershell.exe") {
                "powershell.exe"
            } else {
                "cmd.exe"
            };
            return CommandSpec {
                program: program.into(),
                args: Vec::new(),
                cwd,
            };
        }
        return CommandSpec {
            program: "cmd.exe".into(),
            args: vec!["/C".into(), shell.into()],
            cwd,
        };
    }
    #[cfg(not(windows))]
    {
        if shell.is_empty() {
            let program = unix_shell
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "/bin/sh".into());
            return CommandSpec {
                program,
                args: vec!["-l".into()],
                cwd,
            };
        }
        let program = unix_shell
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "/bin/sh".into());
        CommandSpec {
            program,
            args: vec!["-lc".into(), shell.into()],
            cwd,
        }
    }
}

/// The command the running application would start, for a header summary. Never logged.
pub fn resolve_current(shell: &str, cwd: &str) -> CommandSpec {
    resolve(
        shell,
        cwd,
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var("SHELL").ok(),
        which,
    )
}

fn which(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

/// A short label for the panel header: the shell's file name and the directory.
pub fn summary(spec: &CommandSpec) -> String {
    let shell = PathBuf::from(&spec.program).file_name().map_or_else(
        || spec.program.clone(),
        |s| s.to_string_lossy().into_owned(),
    );
    format!("{shell} · {}", spec.cwd.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_default_is_a_login_shell() {
        let spec = resolve(
            "",
            "",
            Some(PathBuf::from("/home/me")),
            Some("/bin/zsh".into()),
            |_| false,
        );
        assert_eq!(spec.program, "/bin/zsh");
        assert_eq!(spec.args, ["-l"]);
        assert_eq!(spec.cwd, PathBuf::from("/home/me"));
    }

    #[test]
    fn custom_shell_runs_through_the_wrapper() {
        let spec = resolve(
            "exec codex --flag 'a b'",
            "/tmp",
            None,
            Some("/bin/bash".into()),
            |_| false,
        );
        assert_eq!(spec.program, "/bin/bash");
        assert_eq!(spec.args, ["-lc", "exec codex --flag 'a b'"]);
        assert_eq!(spec.cwd, PathBuf::from("/tmp"));
    }

    #[test]
    fn missing_shell_falls_back_to_sh() {
        let spec = resolve("", "", None, None, |_| false);
        assert_eq!(spec.program, "/bin/sh");
        assert_eq!(spec.args, ["-l"]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_fallback_order() {
        let spec = resolve("", "", None, None, |name| name == "powershell.exe");
        assert_eq!(spec.program, "powershell.exe");
        let spec = resolve("", "", None, None, |_| false);
        assert_eq!(spec.program, "cmd.exe");
        let spec = resolve("echo hi", "", None, None, |_| false);
        assert_eq!(spec.program, "cmd.exe");
        assert_eq!(spec.args, ["/C", "echo hi"]);
    }

    #[test]
    fn summary_has_no_shell_arguments() {
        let spec = resolve(
            "secret --password hunter2",
            "",
            None,
            Some("/bin/bash".into()),
            |_| false,
        );
        let text = summary(&spec);
        assert!(!text.contains("hunter2"));
        assert!(text.contains("bash"));
    }
}
