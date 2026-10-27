//! The Terminal workspace: a persistent folder under the app's configuration directory
//! with the person's `AGENTS.md` and the embedded sqlr skills. A worker prepares it once
//! per launch; the UI does no filesystem I/O.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use sqlr_mcp::skills::{SKILLS, Skill};

/// Why the terminal workspace could not be prepared.
#[derive(Debug, thiserror::Error)]
#[error("Could not prepare terminal workspace: {0}")]
struct WorkspaceError(#[source] std::io::Error);

/// The folders sqlr writes its skills into, relative to the workspace: the canonical copy,
/// and the per-skill links Claude Code reads.
const AGENTS_SKILLS: &str = ".agents/skills";
const CLAUDE_SKILLS: &str = ".claude/skills";

/// The skill names sqlr owns in the workspace.
const SKILL_PREFIX: &str = "sqlr-";

/// Staging folders for a skill being rewritten; one left by a crash is removed.
const STAGING_PREFIX: &str = ".sqlr-skill-";

#[derive(Default)]
pub(crate) struct TerminalWorkspace {
    started: bool,
    receiver: Option<mpsc::Receiver<Result<PathBuf, WorkspaceError>>>,
    ready: bool,
    path: Option<PathBuf>,
    error: Option<String>,
}

impl TerminalWorkspace {
    /// Starts once, on the first open of a terminal using the automatic directory.
    pub fn start(&mut self, config_dir: &Path, ctx: &egui::Context) {
        if self.started {
            return;
        }
        self.started = true;
        let (updates, receiver) = mpsc::channel();
        let config_dir = config_dir.to_owned();
        let ctx = ctx.clone();
        let worker = std::thread::Builder::new()
            .name("sqlr-terminal-workspace".into())
            .spawn(move || {
                let _ = updates.send(prepare(&config_dir, SKILLS).map_err(WorkspaceError));
                ctx.request_repaint();
            });
        match worker {
            Ok(_) => self.receiver = Some(receiver),
            Err(error) => {
                self.error = Some(format!(
                    "Could not start terminal workspace worker: {error}"
                ))
            }
        }
    }

    /// Picks up the worker's answer once it arrives.
    pub fn sync(&mut self) {
        let Some(receiver) = &self.receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(path)) => {
                self.path = Some(path);
                self.ready = true;
            }
            Ok(Err(error)) => self.error = Some(error.to_string()),
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.error = Some("The terminal workspace worker stopped".into());
            }
        }
        self.receiver = None;
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

/// Creates the workspace when missing and brings sqlr's files in it up to date.
fn prepare(config_dir: &Path, skills: &[Skill]) -> std::io::Result<PathBuf> {
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
            use std::io::Write;
            file.write_all(include_str!("terminal_agents.md").as_bytes())?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    link_claude_guidance(&directory)?;
    sync_skills(&directory, skills)?;
    remove_generated_mcp_config(&directory);
    Ok(directory)
}

/// Claude Code reads `CLAUDE.md`, not `AGENTS.md`: keep one guide, linked under both names.
/// An existing `CLAUDE.md` (file or link, including a broken one) is never replaced.
fn link_claude_guidance(directory: &Path) -> std::io::Result<()> {
    let path = directory.join("CLAUDE.md");
    match std::fs::symlink_metadata(&path) {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("AGENTS.md", &path)
    }
    #[cfg(not(unix))]
    {
        // Symlinks need a privilege the app usually does not have on Windows; the import
        // directive gives Claude Code the same content without one.
        std::fs::write(&path, "@AGENTS.md\n")
    }
}

/// Writes each skill to `.agents/skills/<name>` with a `.claude/skills/<name>` link (a copy
/// on Windows), rewrites a `sqlr-*` skill whose files differ, and removes `sqlr-*` entries
/// no longer in `skills`. Every other entry is left alone.
fn sync_skills(directory: &Path, skills: &[Skill]) -> std::io::Result<()> {
    let agents = directory.join(AGENTS_SKILLS);
    let claude = directory.join(CLAUDE_SKILLS);
    std::fs::create_dir_all(&agents)?;
    std::fs::create_dir_all(&claude)?;
    for skill in skills {
        write_skill(&agents, skill)?;
        #[cfg(unix)]
        {
            let link = claude.join(skill.name);
            let target = Path::new("../..").join(AGENTS_SKILLS).join(skill.name);
            if std::fs::read_link(&link).ok().as_deref() != Some(target.as_path()) {
                remove_entry(&link)?;
                std::os::unix::fs::symlink(&target, &link)?;
            }
        }
        #[cfg(not(unix))]
        write_skill(&claude, skill)?;
    }
    for parent in [&agents, &claude] {
        for entry in std::fs::read_dir(parent)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let stale =
                name.starts_with(SKILL_PREFIX) && !skills.iter().any(|skill| skill.name == name);
            if stale || name.starts_with(STAGING_PREFIX) {
                remove_entry(&entry.path())?;
            }
        }
    }
    Ok(())
}

/// Writes `skill` into `parent/<name>` unless a folder with exactly its files is there. The
/// new copy is staged beside it and renamed into place.
fn write_skill(parent: &Path, skill: &Skill) -> std::io::Result<()> {
    let target = parent.join(skill.name);
    let expected: BTreeMap<PathBuf, &str> = skill
        .files
        .iter()
        .map(|file| (skill_path(file.path), file.contents))
        .collect();
    if read_folder(&target).is_some_and(|found| {
        found.len() == expected.len()
            && expected
                .iter()
                .all(|(path, contents)| found.get(path).map(String::as_str) == Some(*contents))
    }) {
        return Ok(());
    }
    let staging = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(parent)?;
    for (path, contents) in &expected {
        let path = staging.path().join(path);
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        std::fs::write(path, contents)?;
    }
    remove_entry(&target)?;
    std::fs::rename(staging.path(), &target)?;
    let _ = staging.keep();
    Ok(())
}

/// A `/`-separated skill file path as a relative native path.
fn skill_path(path: &str) -> PathBuf {
    path.split('/').collect()
}

/// Every regular file under `folder` by relative path, or `None` when `folder` is not a
/// plain folder of readable text files.
fn read_folder(folder: &Path) -> Option<BTreeMap<PathBuf, String>> {
    fn walk(root: &Path, folder: &Path, out: &mut BTreeMap<PathBuf, String>) -> Option<()> {
        for entry in std::fs::read_dir(folder).ok()? {
            let path = entry.ok()?.path();
            let kind = std::fs::symlink_metadata(&path).ok()?.file_type();
            if kind.is_dir() {
                walk(root, &path, out)?;
            } else if kind.is_file() {
                let contents = std::fs::read_to_string(&path).ok()?;
                out.insert(path.strip_prefix(root).ok()?.to_owned(), contents);
            } else {
                return None;
            }
        }
        Some(())
    }
    if !std::fs::symlink_metadata(folder).ok()?.is_dir() {
        return None;
    }
    let mut files = BTreeMap::new();
    walk(folder, folder, &mut files)?;
    Some(files)
}

/// Removes a file, link or folder; a missing entry is fine.
fn remove_entry(path: &Path) -> std::io::Result<()> {
    let kind = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if kind.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        // A directory link on Windows is removed as a directory.
        std::fs::remove_file(path).or_else(|error| std::fs::remove_dir(path).map_err(|_| error))
    }
}

/// Earlier versions wrote a project `.mcp.json` holding the token. It is deleted when it
/// still holds only that: a `sqlr` entry or an empty server map. Anything else, and a file
/// that cannot be read, stays.
fn remove_generated_mcp_config(directory: &Path) {
    let path = directory.join(".mcp.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(serde_json::Value::Object(root)) = serde_json::from_str(&text) else {
        return;
    };
    let generated = root.len() == 1
        && root
            .get("mcpServers")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|servers| {
                servers.is_empty() || (servers.len() == 1 && servers.contains_key("sqlr"))
            });
    if generated && let Err(error) = std::fs::remove_file(&path) {
        tracing::warn!(%error, "could not remove the generated terminal .mcp.json");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_mcp::skills::SkillFile;
    use std::time::{Duration, Instant};

    fn wait_ready(workspace: &mut TerminalWorkspace) -> PathBuf {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            workspace.sync();
            assert!(workspace.error().is_none(), "{:?}", workspace.error());
            if workspace.ready() {
                return workspace.path().unwrap().to_owned();
            }
            assert!(Instant::now() < deadline, "workspace did not become ready");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn assert_skill_written(directory: &Path, skill: &Skill) {
        let folder = directory.join(AGENTS_SKILLS).join(skill.name);
        let found = read_folder(&folder).unwrap();
        assert_eq!(found.len(), skill.files.len(), "{}", skill.name);
        for file in skill.files {
            assert_eq!(
                found[&skill_path(file.path)],
                file.contents,
                "{}",
                file.path
            );
            assert_eq!(
                std::fs::read_to_string(
                    directory
                        .join(CLAUDE_SKILLS)
                        .join(skill.name)
                        .join(skill_path(file.path))
                )
                .unwrap(),
                file.contents
            );
        }
    }

    #[test]
    fn workspace_files_persist_across_app_sessions_without_being_overwritten() {
        let ctx = egui::Context::default();
        let config_dir = tempfile::tempdir().unwrap();
        let mut first = TerminalWorkspace::default();
        assert!(first.path().is_none());
        first.start(config_dir.path(), &ctx);
        let first_path = wait_ready(&mut first);
        assert_eq!(first_path, config_dir.path().join("terminal"));
        std::fs::write(first_path.join("AGENTS.md"), "user additions").unwrap();
        std::fs::write(first_path.join("scratch.sql"), "select 1").unwrap();
        drop(first);
        let mut second = TerminalWorkspace::default();
        second.start(config_dir.path(), &ctx);
        let second_path = wait_ready(&mut second);
        assert_eq!(first_path, second_path);
        assert_eq!(
            std::fs::read_to_string(second_path.join("AGENTS.md")).unwrap(),
            "user additions"
        );
        assert_eq!(
            std::fs::read_to_string(second_path.join("scratch.sql")).unwrap(),
            "select 1"
        );
    }

    #[test]
    fn a_missing_agents_file_gets_the_short_template() {
        let config_dir = tempfile::tempdir().unwrap();
        let directory = prepare(config_dir.path(), SKILLS).unwrap();
        let guide = std::fs::read_to_string(directory.join("AGENTS.md")).unwrap();
        assert_eq!(guide, include_str!("terminal_agents.md"));
        for required in ["Settings → Agents", "Install", "sqlr-*", "never rewrites"] {
            assert!(guide.contains(required), "missing {required}: {guide}");
        }
        for stale in ["get_guide", ".mcp.json", "Bearer"] {
            assert!(!guide.contains(stale), "stale {stale}: {guide}");
        }
    }

    #[test]
    fn the_embedded_skills_are_written_with_per_skill_links() {
        let config_dir = tempfile::tempdir().unwrap();
        let directory = prepare(config_dir.path(), SKILLS).unwrap();
        for skill in SKILLS {
            assert_skill_written(&directory, skill);
        }
        assert!(
            directory
                .join(AGENTS_SKILLS)
                .join("sqlr-query-database/references/sql.md")
                .is_file()
        );
        #[cfg(unix)]
        for skill in SKILLS {
            let link = directory.join(CLAUDE_SKILLS).join(skill.name);
            assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
            assert_eq!(
                std::fs::read_link(&link).unwrap(),
                Path::new("../../.agents/skills").join(skill.name)
            );
        }
        let entries: Vec<_> = std::fs::read_dir(directory.join(AGENTS_SKILLS))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries.len(),
            SKILLS.len(),
            "no lock or staging files: {entries:?}"
        );
        assert!(!directory.join(".mcp.json").exists());
    }

    #[test]
    fn a_changed_sqlr_skill_is_rewritten_on_the_next_preparation() {
        let config_dir = tempfile::tempdir().unwrap();
        let directory = prepare(config_dir.path(), SKILLS).unwrap();
        let folder = directory.join(AGENTS_SKILLS).join("sqlr-query-database");
        std::fs::write(folder.join("SKILL.md"), "edited").unwrap();
        std::fs::write(folder.join("references/extra.md"), "extra").unwrap();
        std::fs::remove_file(folder.join("references/redis.md")).unwrap();
        #[cfg(unix)]
        {
            let link = directory.join(CLAUDE_SKILLS).join("sqlr-configure");
            std::fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink("elsewhere", &link).unwrap();
        }
        let modified = std::fs::metadata(
            directory
                .join(AGENTS_SKILLS)
                .join("sqlr-connect-agent/SKILL.md"),
        )
        .unwrap()
        .modified()
        .unwrap();
        prepare(config_dir.path(), SKILLS).unwrap();
        for skill in SKILLS {
            assert_skill_written(&directory, skill);
        }
        assert_eq!(
            std::fs::metadata(
                directory
                    .join(AGENTS_SKILLS)
                    .join("sqlr-connect-agent/SKILL.md")
            )
            .unwrap()
            .modified()
            .unwrap(),
            modified,
            "an unchanged skill is not rewritten"
        );
    }

    #[test]
    fn stale_sqlr_skills_are_removed_and_other_skills_are_left_alone() {
        static FILES: &[SkillFile] = &[SkillFile {
            path: "SKILL.md",
            contents: "old",
        }];
        static OLD: &[Skill] = &[Skill {
            name: "sqlr-retired",
            files: FILES,
        }];
        let config_dir = tempfile::tempdir().unwrap();
        let directory = prepare(config_dir.path(), OLD).unwrap();
        let agents = directory.join(AGENTS_SKILLS);
        let claude = directory.join(CLAUDE_SKILLS);
        std::fs::create_dir_all(agents.join("my-skill")).unwrap();
        std::fs::write(agents.join("my-skill/SKILL.md"), "mine").unwrap();
        std::fs::create_dir_all(claude.join("other-skill")).unwrap();
        std::fs::write(claude.join("other-skill/SKILL.md"), "theirs").unwrap();
        std::fs::create_dir_all(agents.join(".sqlr-skill-crashed")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../../.agents/skills/my-skill", claude.join("my-skill"))
            .unwrap();
        prepare(config_dir.path(), SKILLS).unwrap();
        assert!(!agents.join("sqlr-retired").exists());
        assert!(std::fs::symlink_metadata(claude.join("sqlr-retired")).is_err());
        assert!(!agents.join(".sqlr-skill-crashed").exists());
        assert_eq!(
            std::fs::read_to_string(agents.join("my-skill/SKILL.md")).unwrap(),
            "mine"
        );
        assert_eq!(
            std::fs::read_to_string(claude.join("other-skill/SKILL.md")).unwrap(),
            "theirs"
        );
        #[cfg(unix)]
        assert_eq!(
            std::fs::read_link(claude.join("my-skill")).unwrap(),
            Path::new("../../.agents/skills/my-skill")
        );
        for skill in SKILLS {
            assert_skill_written(&directory, skill);
        }
    }

    #[test]
    fn a_generated_mcp_config_is_deleted_and_an_edited_one_is_kept() {
        let generated = [
            r#"{"mcpServers":{}}"#,
            r#"{"mcpServers":{"sqlr":{"url":"http://127.0.0.1:47821/mcp","headers":{"Authorization":"Bearer old"}}}}"#,
        ];
        for config in generated {
            let config_dir = tempfile::tempdir().unwrap();
            let directory = config_dir.path().join("terminal");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(".mcp.json"), config).unwrap();
            prepare(config_dir.path(), SKILLS).unwrap();
            assert!(!directory.join(".mcp.json").exists(), "{config}");
        }
        let edited = [
            r#"{"mcpServers":{"sqlr":{"url":"x"},"other":{"command":"tool"}}}"#,
            r#"{"mcpServers":{"other":{"command":"tool"}}}"#,
            r#"{"mcpServers":{},"extra":true}"#,
            "not json",
        ];
        for config in edited {
            let config_dir = tempfile::tempdir().unwrap();
            let directory = config_dir.path().join("terminal");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(".mcp.json"), config).unwrap();
            prepare(config_dir.path(), SKILLS).unwrap();
            assert_eq!(
                std::fs::read_to_string(directory.join(".mcp.json")).unwrap(),
                config
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn claude_guidance_links_to_the_agents_file() {
        let config_dir = tempfile::tempdir().unwrap();
        let directory = prepare(config_dir.path(), SKILLS).unwrap();
        let link = directory.join("CLAUDE.md");
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            std::path::PathBuf::from("AGENTS.md")
        );
        assert_eq!(
            std::fs::read_to_string(&link).unwrap(),
            std::fs::read_to_string(directory.join("AGENTS.md")).unwrap()
        );
    }

    #[test]
    fn an_existing_claude_guidance_file_is_never_replaced() {
        let config_dir = tempfile::tempdir().unwrap();
        let directory = config_dir.path().join("terminal");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("AGENTS.md"), "guide").unwrap();
        std::fs::write(directory.join("CLAUDE.md"), "mine").unwrap();
        prepare(config_dir.path(), SKILLS).unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.join("CLAUDE.md")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn a_preparation_failure_is_reported() {
        let ctx = egui::Context::default();
        let config_dir = tempfile::tempdir().unwrap();
        // A file where the workspace folder should be makes the preparation fail.
        std::fs::write(config_dir.path().join("terminal"), "").unwrap();
        let mut workspace = TerminalWorkspace::default();
        workspace.start(config_dir.path(), &ctx);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            workspace.sync();
            if let Some(error) = workspace.error() {
                assert!(error.contains("Could not prepare terminal workspace"));
                break;
            }
            assert!(Instant::now() < deadline, "failure was not reported");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!workspace.ready());
        // Retry starts a fresh preparation, which recovers once the obstacle is gone.
        std::fs::remove_file(config_dir.path().join("terminal")).unwrap();
        let mut workspace = TerminalWorkspace::default();
        workspace.start(config_dir.path(), &ctx);
        wait_ready(&mut workspace);
    }

    #[test]
    fn workspace_errors_keep_their_messages() {
        let prepare = WorkspaceError(std::io::Error::other("denied"));
        assert_eq!(
            prepare.to_string(),
            "Could not prepare terminal workspace: denied"
        );
    }
}
