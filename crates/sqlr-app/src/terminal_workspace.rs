//! Persistent terminal files under the app's configuration directory. A worker prepares
//! the directory and writes credentials atomically; the UI does no filesystem I/O.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

const EMPTY_CONFIG: &str = "{\"mcpServers\":{}}\n";

struct ConfigUpdate {
    revision: u64,
    config: Option<String>,
}

struct WorkspaceUpdate {
    revision: u64,
    result: Result<PathBuf, String>,
}

#[derive(Default)]
pub(crate) struct TerminalWorkspace {
    sender: Option<mpsc::Sender<ConfigUpdate>>,
    receiver: Option<mpsc::Receiver<WorkspaceUpdate>>,
    config: Option<String>,
    revision: u64,
    ready: bool,
    path: Option<PathBuf>,
    error: Option<String>,
}

impl TerminalWorkspace {
    /// Starts once, on the first open of a terminal using the automatic directory.
    pub fn start(
        &mut self,
        config_dir: &Path,
        data_dir: &Path,
        config: Option<String>,
        ctx: &egui::Context,
    ) {
        if self.sender.is_some() || self.error.is_some() {
            return;
        }
        let (sender, requests) = mpsc::channel::<ConfigUpdate>();
        let (updates, receiver) = mpsc::channel();
        let data_dir = data_dir.to_owned();
        let config_dir = config_dir.to_owned();
        let ctx = ctx.clone();
        let worker = std::thread::Builder::new()
            .name("sqlr-terminal-workspace".into())
            .spawn(move || {
                let directory = match create_workspace(&config_dir, &data_dir) {
                    Ok(directory) => directory,
                    Err(error) => {
                        let _ = updates.send(WorkspaceUpdate {
                            revision: 0,
                            result: Err(format!("Could not prepare terminal workspace: {error}")),
                        });
                        ctx.request_repaint();
                        return;
                    }
                };
                while let Ok(mut update) = requests.recv() {
                    // Only the newest queued credentials matter.
                    for newer in requests.try_iter() {
                        update = newer;
                    }
                    let result = write_config(&directory, update.config.as_deref())
                        .map(|()| directory.clone())
                        .map_err(|error| format!("Could not write terminal MCP config: {error}"));
                    if updates
                        .send(WorkspaceUpdate {
                            revision: update.revision,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
        if let Err(error) = worker {
            self.error = Some(format!(
                "Could not start terminal workspace worker: {error}"
            ));
            return;
        }
        self.sender = Some(sender);
        self.receiver = Some(receiver);
        self.config = config.clone();
        self.send(config);
    }

    /// Also called while hidden, so rotation and disabling MCP refresh the file.
    pub fn sync(&mut self, config: Option<String>) {
        if self.sender.is_none() {
            return;
        }
        if config != self.config {
            self.config = config.clone();
            self.revision += 1;
            self.ready = false;
            self.error = None;
            self.send(config);
        }
        if let Some(receiver) = &self.receiver {
            loop {
                let update = match receiver.try_recv() {
                    Ok(update) => update,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.error
                            .get_or_insert_with(|| "The terminal workspace worker stopped".into());
                        break;
                    }
                };
                if update.revision != self.revision {
                    continue;
                }
                match update.result {
                    Ok(path) => {
                        self.path = Some(path);
                        self.error = None;
                        self.ready = true;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        }
    }

    fn send(&mut self, config: Option<String>) {
        if let Some(sender) = &self.sender
            && sender
                .send(ConfigUpdate {
                    revision: self.revision,
                    config,
                })
                .is_err()
        {
            self.error = Some("The terminal workspace worker stopped".into());
        }
    }

    pub fn ready(&self) -> bool {
        self.ready
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

fn create_workspace(config_dir: &Path, data_dir: &Path) -> std::io::Result<PathBuf> {
    let directory = config_dir.join("terminal");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&directory)?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("AGENTS.md"))
    {
        Ok(mut file) => {
            writeln!(
                file,
                "{}\n## Server discovery\n\nThe running application's discovery file is `{}`. \
                 It contains the URL and process metadata, without credentials.",
                include_str!("terminal_agents.md"),
                sqlr_mcp::discovery::Discovery::path(data_dir).display(),
            )?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    Ok(directory)
}

fn write_config(directory: &Path, config: Option<&str>) -> std::io::Result<()> {
    let config = config.unwrap_or(EMPTY_CONFIG);
    let path = directory.join(".mcp.json");
    if std::fs::read_to_string(&path).is_ok_and(|existing| existing == config) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        return Ok(());
    }
    // NamedTempFile starts at mode 0600 on Unix. Rename preserves those permissions and
    // prevents a client from observing partially written JSON or credentials.
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(config.as_bytes())?;
    file.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_ready(workspace: &mut TerminalWorkspace, config: Option<&str>) -> PathBuf {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            workspace.sync(config.map(str::to_owned));
            assert!(workspace.error().is_none(), "{:?}", workspace.error());
            if workspace.ready() {
                return workspace.path().unwrap().to_owned();
            }
            assert!(Instant::now() < deadline, "workspace did not become ready");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn workspace_files_persist_across_app_sessions_without_being_overwritten() {
        let ctx = egui::Context::default();
        let config_dir = tempfile::tempdir().unwrap();
        let mut first = TerminalWorkspace::default();
        assert!(first.path().is_none());
        first.start(config_dir.path(), config_dir.path(), None, &ctx);
        let first_path = wait_ready(&mut first, None);
        assert_eq!(first_path, config_dir.path().join("terminal"));
        std::fs::write(first_path.join("AGENTS.md"), "user additions").unwrap();
        std::fs::write(first_path.join("scratch.sql"), "select 1").unwrap();
        let config_modified = std::fs::metadata(first_path.join(".mcp.json"))
            .unwrap()
            .modified()
            .unwrap();
        drop(first);
        let mut second = TerminalWorkspace::default();
        second.start(config_dir.path(), config_dir.path(), None, &ctx);
        let second_path = wait_ready(&mut second, None);
        assert_eq!(first_path, second_path);
        assert_eq!(
            std::fs::read_to_string(second_path.join("AGENTS.md")).unwrap(),
            "user additions"
        );
        assert_eq!(
            std::fs::read_to_string(second_path.join("scratch.sql")).unwrap(),
            "select 1"
        );
        assert_eq!(
            std::fs::metadata(second_path.join(".mcp.json"))
                .unwrap()
                .modified()
                .unwrap(),
            config_modified
        );
    }

    #[test]
    fn token_rotation_and_port_changes_replace_config_without_rewriting_user_files() {
        let ctx = egui::Context::default();
        let config_dir = tempfile::tempdir().unwrap();
        let mut workspace = TerminalWorkspace::default();
        let old = r#"{"mcpServers":{"sqlr":{"url":"http://127.0.0.1:47821/mcp","headers":{"Authorization":"Bearer old-token"}}}}"#;
        let new = r#"{"mcpServers":{"sqlr":{"url":"http://127.0.0.1:47822/mcp","headers":{"Authorization":"Bearer new-token"}}}}"#;
        workspace.start(config_dir.path(), config_dir.path(), Some(old.into()), &ctx);
        let path = wait_ready(&mut workspace, Some(old));
        std::fs::write(path.join("AGENTS.md"), "user additions").unwrap();
        std::fs::write(path.join("scratch.sql"), "select 1").unwrap();
        // Exercise queued updates as well as the normal ready -> pending transition.
        workspace.sync(None);
        workspace.sync(Some(new.into()));
        wait_ready(&mut workspace, Some(new));
        assert_eq!(
            std::fs::read_to_string(path.join(".mcp.json")).unwrap(),
            new
        );
        assert_eq!(
            std::fs::read_to_string(path.join("AGENTS.md")).unwrap(),
            "user additions"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("scratch.sql")).unwrap(),
            "select 1"
        );
    }

    #[test]
    fn config_write_failures_are_reported_and_can_recover_on_the_next_update() {
        let ctx = egui::Context::default();
        let config_dir = tempfile::tempdir().unwrap();
        let mut workspace = TerminalWorkspace::default();
        workspace.start(config_dir.path(), config_dir.path(), None, &ctx);
        let path = wait_ready(&mut workspace, None);
        std::fs::remove_file(path.join(".mcp.json")).unwrap();
        std::fs::create_dir(path.join(".mcp.json")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            workspace.sync(Some(EMPTY_CONFIG.into()));
            if let Some(error) = workspace.error() {
                assert!(error.contains("Could not write terminal MCP config"));
                break;
            }
            assert!(Instant::now() < deadline, "write failure was not reported");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!workspace.ready());
        std::fs::remove_dir(path.join(".mcp.json")).unwrap();
        wait_ready(&mut workspace, None);
        assert_eq!(
            std::fs::read_to_string(path.join(".mcp.json")).unwrap(),
            EMPTY_CONFIG
        );
    }
}
