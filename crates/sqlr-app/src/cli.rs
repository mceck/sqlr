//! The command line: `sqlr [--data-dir <path>] [--config-dir <path>]`, `--version` and
//! `--help`, parsed by hand.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliCommand {
    /// Start the app, with the data and config directories when given.
    Run {
        data_dir: Option<PathBuf>,
        config_dir: Option<PathBuf>,
    },
    Version,
    Help,
    /// A usage error: the message for stderr (the binary then prints the usage, exit 2).
    Error(String),
}

pub const USAGE: &str = "\
Usage: sqlr [OPTIONS]

Options:
      --data-dir <path>    Keep history, snippets, logs and known hosts in <path>
      --config-dir <path>  Read connections, profiles and settings from <path>
  -V, --version            Print the version
  -h, --help               Print this help
";

/// `sqlr <version>`.
pub fn version_line() -> String {
    format!("sqlr {}", env!("CARGO_PKG_VERSION"))
}

/// Parses the arguments after the program name. `--opt=value` and `--opt value` both work.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> CliCommand {
    let mut data_dir = None;
    let mut config_dir = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag.to_string(), Some(value)),
            _ => (arg.clone(), None),
        };
        let slot = match flag.as_str() {
            "-V" | "--version" if inline.is_none() => return CliCommand::Version,
            "-h" | "--help" if inline.is_none() => return CliCommand::Help,
            "--data-dir" => &mut data_dir,
            "--config-dir" => &mut config_dir,
            _ => return CliCommand::Error(format!("unknown option: {arg}")),
        };
        let value = match inline {
            Some(value) => value.to_string(),
            None => match args.next() {
                Some(value) if !value.starts_with("--") => value,
                _ => return CliCommand::Error(format!("{flag} needs a path")),
            },
        };
        if value.is_empty() {
            return CliCommand::Error(format!("{flag} needs a path"));
        }
        *slot = Some(PathBuf::from(value));
    }
    CliCommand::Run {
        data_dir,
        config_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> CliCommand {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parse_version_help_dirs_and_errors() {
        assert_eq!(
            parse(&[]),
            CliCommand::Run {
                data_dir: None,
                config_dir: None
            }
        );
        assert_eq!(parse(&["--version"]), CliCommand::Version);
        assert_eq!(parse(&["-V"]), CliCommand::Version);
        assert_eq!(parse(&["--help"]), CliCommand::Help);
        assert_eq!(parse(&["-h"]), CliCommand::Help);
        assert_eq!(
            parse(&["--data-dir", "/tmp/d", "--config-dir=/tmp/c"]),
            CliCommand::Run {
                data_dir: Some("/tmp/d".into()),
                config_dir: Some("/tmp/c".into())
            }
        );
        assert!(
            matches!(parse(&["--frobnicate"]), CliCommand::Error(e) if e.contains("--frobnicate"))
        );
        assert!(
            matches!(parse(&["--data-dir"]), CliCommand::Error(e) if e.contains("needs a path"))
        );
        assert!(matches!(
            parse(&["--data-dir", "--help"]),
            CliCommand::Error(_)
        ));
        assert!(matches!(parse(&["--config-dir="]), CliCommand::Error(_)));
        assert!(matches!(parse(&["--version=1"]), CliCommand::Error(_)));
        assert!(matches!(parse(&["stray"]), CliCommand::Error(_)));
        assert!(version_line().starts_with("sqlr "));
    }
}
