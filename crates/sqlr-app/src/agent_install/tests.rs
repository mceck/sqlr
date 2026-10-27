//! The installation seam: discovery, state, install, update, remove and rewrite against a
//! temporary home with fixture files in each agent's format.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;

const URL: &str = "http://127.0.0.1:47821/mcp";
const TOKEN: &str = "tok-first";

struct Home {
    _dir: tempfile::TempDir,
    data: PathBuf,
    env: AgentEnvironment,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        fs::create_dir_all(&home).unwrap();
        Self {
            env: AgentEnvironment::in_home(&home),
            data,
            _dir: dir,
        }
    }

    fn installations(&self) -> Installations {
        Installations::new(self.env.clone(), &self.data)
    }

    fn file(&self, kind: AgentKind) -> PathBuf {
        kind.config_file(&self.env)
    }

    /// Writes the agent's configuration file (and so makes it detected).
    fn write(&self, kind: AgentKind, text: &str) -> PathBuf {
        let path = self.file(kind);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    fn state(&self, kind: AgentKind, endpoint: &Endpoint) -> AgentState {
        self.installations()
            .rows(endpoint)
            .into_iter()
            .find(|row| row.kind == kind)
            .unwrap()
            .state
    }
}

fn endpoint() -> Endpoint {
    Endpoint::running(URL, TOKEN)
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn json_of(path: &Path) -> Value {
    jsonc_parser::cst::CstRootNode::parse(&read(path), &Default::default())
        .unwrap()
        .to_serde_value()
        .unwrap()
}

#[test]
fn an_empty_home_detects_no_agent() {
    let home = Home::new();
    for row in home.installations().rows(&endpoint()) {
        assert_eq!(row.state, AgentState::NotDetected, "{:?}", row.kind);
        assert!(!row.recorded);
    }
}

#[test]
fn agents_are_detected_by_configuration_directory_or_installation_path() {
    let mut home = Home::new();
    fs::create_dir_all(home.env.home.join(".codex")).unwrap();
    fs::create_dir_all(home.env.home.join(".local/bin")).unwrap();
    fs::write(home.env.home.join(".local/bin/gemini"), "").unwrap();
    fs::create_dir_all(home.env.home.join(".claude/local")).unwrap();
    fs::create_dir_all(home.env.config_dir.join("Code/User")).unwrap();
    let apps = home.env.home.join("apps");
    fs::create_dir_all(apps.join("Cursor.app")).unwrap();
    home.env.app_dirs = vec![apps];
    let bins = home.env.home.join("system-bin");
    fs::create_dir_all(&bins).unwrap();
    fs::write(bins.join("opencode"), "").unwrap();
    home.env.bin_dirs = vec![bins];

    for kind in AgentKind::ALL {
        assert!(home.env.detects(kind), "{kind:?} is detected");
        assert_eq!(home.state(kind, &endpoint()), AgentState::NotInstalled);
    }
}

#[test]
fn install_creates_a_missing_file_owner_only_with_the_entry() {
    let home = Home::new();
    fs::create_dir_all(home.env.home.join(".claude")).unwrap();
    let installations = home.installations();
    installations
        .install(AgentKind::ClaudeCode, &endpoint())
        .unwrap();

    let file = home.file(AgentKind::ClaudeCode);
    assert_eq!(
        json_of(&file),
        json!({"mcpServers": {"sqlr": {
            "type": "http",
            "url": URL,
            "headers": {"Authorization": "Bearer tok-first"}
        }}})
    );
    assert!(read(&file).ends_with('\n'));
    assert!(
        !files::backup_path(&file).exists(),
        "there is nothing to back up"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert_eq!(
        home.state(AgentKind::ClaudeCode, &endpoint()),
        AgentState::Installed
    );
    assert_eq!(
        installations.records(),
        vec![Record {
            agent: AgentKind::ClaudeCode,
            file
        }]
    );
    let registry = read(&home.data.join(REGISTRY_FILE));
    assert!(
        !registry.contains(TOKEN),
        "the registry never holds the token"
    );
}

#[test]
fn install_into_an_empty_file_writes_an_object() {
    let home = Home::new();
    let file = home.write(AgentKind::Cursor, "");
    home.installations()
        .install(AgentKind::Cursor, &endpoint())
        .unwrap();
    assert_eq!(
        json_of(&file),
        json!({"mcpServers": {"sqlr": {
            "url": URL,
            "headers": {"Authorization": "Bearer tok-first"}
        }}})
    );
}

#[test]
fn every_json_agent_gets_its_own_entry_shape() {
    let home = Home::new();
    let cases = [
        (
            AgentKind::GeminiCli,
            json!({"mcpServers": {"sqlr": {
                "httpUrl": URL, "headers": {"Authorization": "Bearer tok-first"}
            }}}),
        ),
        (
            AgentKind::VsCode,
            json!({"servers": {"sqlr": {
                "type": "http", "url": URL, "headers": {"Authorization": "Bearer tok-first"}
            }}}),
        ),
        (
            AgentKind::OpenCode,
            json!({"mcp": {"sqlr": {
                "type": "remote", "url": URL, "headers": {"Authorization": "Bearer tok-first"}
            }}}),
        ),
    ];
    for (kind, expected) in cases {
        let file = home.write(kind, "{}\n");
        home.installations().install(kind, &endpoint()).unwrap();
        assert_eq!(json_of(&file), expected, "{kind:?}");
        assert_eq!(home.state(kind, &endpoint()), AgentState::Installed);
    }
}

#[test]
fn install_keeps_comments_order_and_unrelated_keys_in_jsonc() {
    let home = Home::new();
    let original = r#"{
  // My servers, by hand.
  "servers": {
    "github": {
      "type": "http",
      "url": "https://api.githubcopilot.com/mcp/", // remote
    },
  },
  /* inputs stay */
  "inputs": [],
}
"#;
    let file = home.write(AgentKind::VsCode, original);
    home.installations()
        .install(AgentKind::VsCode, &endpoint())
        .unwrap();
    let text = read(&file);
    assert!(text.contains("// My servers, by hand."));
    assert!(text.contains("\"url\": \"https://api.githubcopilot.com/mcp/\", // remote"));
    assert!(text.contains("/* inputs stay */"));
    assert!(text.find("\"github\"").unwrap() < text.find("\"sqlr\"").unwrap());
    assert!(text.find("\"sqlr\"").unwrap() < text.find("\"inputs\"").unwrap());
    let value = json_of(&file);
    assert_eq!(value["servers"]["github"]["type"], "http");
    assert_eq!(value["inputs"], json!([]));
    assert_eq!(value["servers"]["sqlr"]["url"], URL);
    assert_eq!(
        read(&files::backup_path(&file)),
        original,
        "the backup is the file before the first change"
    );
}

#[test]
fn install_keeps_toml_formatting_and_writes_the_codex_entry() {
    let home = Home::new();
    let original = r#"# Codex settings
model = "o3"  # the default

[mcp_servers.docs]
command = "docs-mcp"
args = ["--port", "1"]

[profiles.fast]
model = "mini"
"#;
    let file = home.write(AgentKind::CodexCli, original);
    home.installations()
        .install(AgentKind::CodexCli, &endpoint())
        .unwrap();
    let text = read(&file);
    assert!(
        text.starts_with(
            "# Codex settings\nmodel = \"o3\"  # the default\n\n[mcp_servers.docs]\ncommand = \"docs-mcp\"\nargs = [\"--port\", \"1\"]\n"
        ),
        "{text}"
    );
    assert!(text.contains("[profiles.fast]\nmodel = \"mini\"\n"));
    assert!(text.contains("[mcp_servers.sqlr]"));
    let value: toml::Table = text.parse().unwrap();
    let sqlr = &value["mcp_servers"]["sqlr"];
    assert_eq!(sqlr["url"].as_str(), Some(URL));
    assert_eq!(
        sqlr["http_headers"]["Authorization"].as_str(),
        Some("Bearer tok-first")
    );
    assert_eq!(
        value["mcp_servers"]["docs"]["command"].as_str(),
        Some("docs-mcp")
    );
    assert_eq!(read(&files::backup_path(&file)), original);
    assert_eq!(
        home.state(AgentKind::CodexCli, &endpoint()),
        AgentState::Installed
    );
}

#[test]
fn a_changed_token_reads_outdated_and_update_repairs_it_keeping_the_first_backup() {
    let home = Home::new();
    let original = "{\n  \"theme\": \"dark\"\n}\n";
    let file = home.write(AgentKind::GeminiCli, original);
    let installations = home.installations();
    installations
        .install(AgentKind::GeminiCli, &endpoint())
        .unwrap();
    let rotated = Endpoint::running(URL, "tok-second");
    assert_eq!(
        home.state(AgentKind::GeminiCli, &rotated),
        AgentState::Outdated
    );

    installations
        .install(AgentKind::GeminiCli, &rotated)
        .unwrap();
    assert_eq!(
        home.state(AgentKind::GeminiCli, &rotated),
        AgentState::Installed
    );
    let text = read(&file);
    assert!(text.contains("Bearer tok-second"));
    assert!(!text.contains("tok-first"));
    assert_eq!(json_of(&file)["theme"], "dark");
    assert_eq!(
        read(&files::backup_path(&file)),
        original,
        "the backup is written once and never overwritten"
    );
}

#[test]
fn update_keeps_keys_the_person_added_to_the_entry() {
    let home = Home::new();
    let file = home.write(
        AgentKind::CodexCli,
        "[mcp_servers.sqlr]\nurl = \"http://127.0.0.1:1/mcp\"\nstartup_timeout_sec = 30\n",
    );
    home.installations()
        .install(AgentKind::CodexCli, &endpoint())
        .unwrap();
    let value: toml::Table = read(&file).parse().unwrap();
    let sqlr = &value["mcp_servers"]["sqlr"];
    assert_eq!(sqlr["url"].as_str(), Some(URL));
    assert_eq!(sqlr["startup_timeout_sec"].as_integer(), Some(30));
}

#[test]
fn without_a_running_server_an_entry_is_compared_by_url() {
    let home = Home::new();
    home.write(AgentKind::Cursor, "{}");
    home.installations()
        .install(AgentKind::Cursor, &endpoint())
        .unwrap();
    assert_eq!(
        home.state(AgentKind::Cursor, &Endpoint::stopped(47821)),
        AgentState::Installed
    );
    assert_eq!(
        home.state(AgentKind::Cursor, &Endpoint::stopped(5000)),
        AgentState::Outdated
    );
    assert!(matches!(
        home.installations()
            .install(AgentKind::Cursor, &Endpoint::stopped(47821)),
        Err(InstallError::NotRunning)
    ));
}

#[test]
fn remove_deletes_only_the_sqlr_entry_and_keeps_the_map() {
    let home = Home::new();
    let file = home.write(
        AgentKind::ClaudeCode,
        "{\n  \"numStartups\": 4,\n  \"mcpServers\": {}\n}\n",
    );
    let installations = home.installations();
    installations
        .install(AgentKind::ClaudeCode, &endpoint())
        .unwrap();
    installations.remove(AgentKind::ClaudeCode).unwrap();
    assert_eq!(json_of(&file), json!({"numStartups": 4, "mcpServers": {}}));
    assert_eq!(
        home.state(AgentKind::ClaudeCode, &endpoint()),
        AgentState::NotInstalled
    );
    assert!(installations.records().is_empty());

    let codex = home.write(
        AgentKind::CodexCli,
        "[mcp_servers.docs]\ncommand = \"docs\"\n\n[mcp_servers.sqlr]\nurl = \"x\"\n",
    );
    installations.remove(AgentKind::CodexCli).unwrap();
    assert_eq!(read(&codex), "[mcp_servers.docs]\ncommand = \"docs\"\n");
}

#[test]
fn an_unreadable_file_needs_manual_setup_and_is_never_overwritten() {
    let home = Home::new();
    let broken = "{ \"mcpServers\": { \"x\": ";
    let file = home.write(AgentKind::Cursor, broken);
    assert!(matches!(
        home.state(AgentKind::Cursor, &endpoint()),
        AgentState::ManualSetup(_)
    ));
    assert!(matches!(
        home.installations().install(AgentKind::Cursor, &endpoint()),
        Err(InstallError::Format(_))
    ));
    assert_eq!(read(&file), broken);
    assert!(!files::backup_path(&file).exists());

    home.write(AgentKind::GeminiCli, "{\"mcpServers\": []}");
    assert_eq!(
        home.state(AgentKind::GeminiCli, &endpoint()),
        AgentState::ManualSetup("`mcpServers` is not an object".into())
    );
    home.write(AgentKind::OpenCode, "[1, 2]");
    assert_eq!(
        home.state(AgentKind::OpenCode, &endpoint()),
        AgentState::ManualSetup("the file is not a JSON object".into())
    );

    home.write(
        AgentKind::CodexCli,
        "[mcp_servers.other]\nheaders = { Authorization = \"Bearer leaked-secret\" \n",
    );
    let AgentState::ManualSetup(reason) = home.state(AgentKind::CodexCli, &endpoint()) else {
        panic!("broken TOML needs manual setup");
    };
    assert!(reason.contains("line 2"), "{reason}");
    assert!(
        !reason.contains("leaked-secret"),
        "the reason never quotes the file: {reason}"
    );
}

#[test]
fn rotation_and_port_change_rewrite_every_recorded_installation() {
    let home = Home::new();
    home.write(AgentKind::ClaudeCode, "{}");
    home.write(AgentKind::CodexCli, "");
    home.write(AgentKind::Cursor, "{}");
    let installations = home.installations();
    for kind in [
        AgentKind::ClaudeCode,
        AgentKind::CodexCli,
        AgentKind::Cursor,
    ] {
        installations.install(kind, &endpoint()).unwrap();
    }
    // Cursor dropped sqlr's entry on its own: its record goes, the entry is not re-added.
    installations.remove(AgentKind::Cursor).unwrap();
    installations
        .install(AgentKind::Cursor, &endpoint())
        .unwrap();
    fs::write(home.file(AgentKind::Cursor), "{\"mcpServers\": {}}").unwrap();

    let moved = Endpoint::running("http://127.0.0.1:5000/mcp", "tok-rotated");
    let failures = installations.rewrite_recorded(&moved);
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(
        home.state(AgentKind::ClaudeCode, &moved),
        AgentState::Installed
    );
    assert_eq!(
        home.state(AgentKind::CodexCli, &moved),
        AgentState::Installed
    );
    assert_eq!(
        home.state(AgentKind::Cursor, &moved),
        AgentState::NotInstalled
    );
    let recorded: Vec<AgentKind> = installations
        .records()
        .into_iter()
        .map(|record| record.agent)
        .collect();
    assert_eq!(recorded, vec![AgentKind::ClaudeCode, AgentKind::CodexCli]);
    assert!(read(&home.file(AgentKind::CodexCli)).contains("Bearer tok-rotated"));
}

#[cfg(unix)]
#[test]
fn a_failed_rewrite_leaves_the_installation_outdated() {
    use std::os::unix::fs::PermissionsExt as _;

    let home = Home::new();
    let file = home.write(AgentKind::GeminiCli, "{}");
    let installations = home.installations();
    installations
        .install(AgentKind::GeminiCli, &endpoint())
        .unwrap();
    let dir = file.parent().unwrap().to_path_buf();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let rotated = Endpoint::running(URL, "tok-rotated");
    let failures = installations.rewrite_recorded(&rotated);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].0, AgentKind::GeminiCli);
    assert!(!failures[0].1.to_string().contains("tok-rotated"));
    assert_eq!(
        home.state(AgentKind::GeminiCli, &rotated),
        AgentState::Outdated
    );
    assert_eq!(installations.records().len(), 1, "the record stays");
}

#[cfg(unix)]
#[test]
fn writes_keep_the_files_permissions_and_follow_symbolic_links() {
    use std::os::unix::fs::PermissionsExt as _;

    let home = Home::new();
    let dotfiles = home.env.home.join("dotfiles");
    fs::create_dir_all(&dotfiles).unwrap();
    let real = dotfiles.join("claude.json");
    fs::write(&real, "{}").unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
    let link = home.file(AgentKind::ClaudeCode);
    std::os::unix::fs::symlink(&real, &link).unwrap();

    home.installations()
        .install(AgentKind::ClaudeCode, &endpoint())
        .unwrap();
    assert!(link.symlink_metadata().unwrap().is_symlink());
    assert!(read(&real).contains("\"sqlr\""));
    assert_eq!(
        fs::metadata(&real).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn the_endpoint_never_shows_its_token() {
    let debug = format!("{:?}", endpoint());
    assert!(debug.contains(URL));
    assert!(!debug.contains(TOKEN));
}

#[test]
fn copy_config_is_in_each_agents_format() {
    let claude: Value =
        serde_json::from_str(&AgentKind::ClaudeCode.config_snippet(URL, TOKEN)).unwrap();
    assert_eq!(claude["mcpServers"]["sqlr"]["type"], "http");
    let vscode: Value =
        serde_json::from_str(&AgentKind::VsCode.config_snippet(URL, TOKEN)).unwrap();
    assert_eq!(vscode["servers"]["sqlr"]["url"], URL);
    let codex: toml::Table = AgentKind::CodexCli
        .config_snippet(URL, TOKEN)
        .parse()
        .unwrap();
    assert_eq!(
        codex["mcp_servers"]["sqlr"]["http_headers"]["Authorization"].as_str(),
        Some("Bearer tok-first")
    );
}
