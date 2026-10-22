//! Native dump and restore jobs (spec §8.5): tool discovery, child processes, their logs
//! and progress.
//!
//! The command lines come from [`sqlr_core::tools`]; this module resolves the connection's
//! real endpoint (secrets and SSH tunnel included), finds the tool and runs it off the UI
//! thread. A dump writes a hidden partial file and renames it only when the tool succeeds;
//! a cancelled or failed dump leaves nothing behind.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sqlr_core::{
    DriverError, DumpFormat, DumpOptions, ErrorKind, RestoreOptions, ToolCommand, ToolConnection,
    dump_command, restore_command,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::engine::{Shared, not_connected};
use crate::job::{JobControl, JobOutcome, JobProgress};
use crate::ssh::Tunnel;

const CANCEL_TICK: Duration = Duration::from_millis(100);
const LOG_CHANNEL: usize = 512;

/// A dump or restore the engine runs.
pub(crate) enum ToolWork {
    Dump(DumpOptions),
    Restore(RestoreOptions),
}

impl ToolWork {
    fn kind(&self) -> &'static str {
        match self {
            ToolWork::Dump(_) => "Dump",
            ToolWork::Restore(_) => "Restore",
        }
    }
}

/// What the connection's tool needs, kept apart from the session.
struct Endpoint {
    driver: String,
    conn: ToolConnection,
    /// Kept alive for the tool's lifetime, so an SSH tunnel stays open.
    _tunnel: Option<Arc<Tunnel>>,
}

/// A line of a child process's output.
struct OutputLine {
    text: String,
}

impl Shared {
    pub(crate) async fn run_database_tool(
        self: Arc<Self>,
        job: JobControl,
        conn: String,
        database: String,
        path: PathBuf,
        work: ToolWork,
    ) {
        let outcome = self
            .database_tool(&job, &conn, &database, &path, &work)
            .await;
        self.jobs.finish(job.id, outcome);
        self.repaint();
    }

    async fn database_tool(
        &self,
        job: &JobControl,
        conn: &str,
        database: &str,
        path: &Path,
        work: &ToolWork,
    ) -> JobOutcome {
        job.log(format!("{} started", work.kind()));
        let endpoint = match self.tool_endpoint(conn, database).await {
            Ok(endpoint) => endpoint,
            Err(e) => return JobOutcome::Failed(e),
        };
        let temp = match tempfile::Builder::new().prefix("sqlr-tool-").tempdir() {
            Ok(temp) => temp,
            Err(e) => return JobOutcome::Failed(io_error("create a temporary directory", e)),
        };
        match work {
            ToolWork::Dump(opts) => self.dump(job, &endpoint, path, opts, temp.path()).await,
            ToolWork::Restore(opts) => self.restore(job, &endpoint, path, opts, temp.path()).await,
        }
    }

    async fn dump(
        &self,
        job: &JobControl,
        endpoint: &Endpoint,
        target: &Path,
        opts: &DumpOptions,
        temp_dir: &Path,
    ) -> JobOutcome {
        if opts.schema_only && opts.data_only {
            return JobOutcome::Failed(config_error(
                "Schema only and data only cannot both be selected",
            ));
        }
        if endpoint.driver == "sqlite" && endpoint.conn.path.as_deref() == Some(target) {
            return JobOutcome::Failed(config_error(
                "The dump's target is the connection's own database file",
            ));
        }
        if let Ok(meta) = tokio::fs::symlink_metadata(target).await {
            if opts.format == DumpFormat::Directory {
                if !meta.is_dir() {
                    return JobOutcome::Failed(config_error(
                        "The dump's target exists and is not a directory",
                    ));
                }
                match tokio::fs::read_dir(target).await {
                    Ok(mut entries) => match entries.next_entry().await {
                        Ok(Some(_)) => {
                            return JobOutcome::Failed(config_error(
                                "The dump's target directory is not empty",
                            ));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            return JobOutcome::Failed(io_error(
                                "read the dump's target directory",
                                e,
                            ));
                        }
                    },
                    Err(e) => {
                        return JobOutcome::Failed(io_error("read the dump's target directory", e));
                    }
                }
            } else if meta.is_dir() {
                return JobOutcome::Failed(config_error("The dump's target is a directory"));
            }
        }
        if let Some(parent) = target.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return JobOutcome::Failed(io_error("create the dump's directory", e));
        }
        let partial = partial_path(target);
        remove_path(&partial).await;
        if endpoint.driver == "sqlite" && opts.format == DumpFormat::SqliteBinary {
            return self.sqlite_backup(job, endpoint, target, &partial).await;
        }
        let command = match dump_command(&endpoint.driver, &endpoint.conn, opts, &partial, temp_dir)
        {
            Ok(command) => command,
            Err(message) => return JobOutcome::Failed(config_error(message)),
        };
        let shown = command.display().replace(
            &partial.display().to_string(),
            &target.display().to_string(),
        );
        job.log(format!("$ {shown}"));
        job.log(format!("Writing {}", target.display()));
        let executed = self.run_command(job, &command).await;
        match executed {
            Ok(()) => {
                if !partial.exists() {
                    return JobOutcome::Failed(config_error(format!(
                        "{} produced no output",
                        endpoint.driver
                    )));
                }
                if let Err(e) = clear_target(target, opts.format == DumpFormat::Directory).await {
                    remove_path(&partial).await;
                    return JobOutcome::Failed(io_error("replace the dump's target", e));
                }
                if let Err(e) = tokio::fs::rename(&partial, target).await {
                    remove_path(&partial).await;
                    return JobOutcome::Failed(io_error("move the dump into place", e));
                }
                job.log(format!("Dump written to {}", target.display()));
                JobOutcome::Dumped {
                    path: target.to_path_buf(),
                }
            }
            Err(ToolEnd::Cancelled) => {
                remove_path(&partial).await;
                JobOutcome::Cancelled
            }
            Err(ToolEnd::Failed(e)) => {
                remove_path(&partial).await;
                JobOutcome::Failed(e)
            }
        }
    }

    async fn restore(
        &self,
        job: &JobControl,
        endpoint: &Endpoint,
        input: &Path,
        opts: &RestoreOptions,
        temp_dir: &Path,
    ) -> JobOutcome {
        if opts.schema_only && opts.data_only {
            return JobOutcome::Failed(config_error(
                "Schema only and data only cannot both be selected",
            ));
        }
        if !input.exists() {
            return JobOutcome::Failed(config_error(format!("{} does not exist", input.display())));
        }
        if endpoint.driver == "sqlite" && !sqlr_core::is_sql_file(input) {
            return self.sqlite_restore(job, endpoint, input).await;
        }
        let command = match restore_command(&endpoint.driver, &endpoint.conn, opts, input, temp_dir)
        {
            Ok(command) => command,
            Err(message) => return JobOutcome::Failed(config_error(message)),
        };
        job.log(format!("$ {}", command.display()));
        job.log(format!("Restoring from {}", input.display()));
        match self.run_command(job, &command).await {
            Ok(()) => {
                job.log("Restore complete");
                JobOutcome::Restored {
                    path: input.to_path_buf(),
                }
            }
            Err(ToolEnd::Cancelled) => JobOutcome::Cancelled,
            Err(ToolEnd::Failed(e)) => JobOutcome::Failed(e),
        }
    }

    /// Copies the SQLite file with the online backup API, while the database stays in use,
    /// into `partial` and then over `target`.
    async fn sqlite_backup(
        &self,
        job: &JobControl,
        endpoint: &Endpoint,
        target: &Path,
        partial: &Path,
    ) -> JobOutcome {
        let Some(db) = endpoint.conn.path.clone() else {
            return JobOutcome::Failed(config_error("The SQLite connection has no database file"));
        };
        job.log(format!("Backing up {} (online backup API)", db.display()));
        let ran = self.sqlite_copy(job, db, partial.to_path_buf()).await;
        match ran {
            Ok(()) => {
                if let Err(e) = clear_target(target, false).await {
                    remove_path(partial).await;
                    return JobOutcome::Failed(io_error("replace the dump's target", e));
                }
                if let Err(e) = tokio::fs::rename(partial, target).await {
                    remove_path(partial).await;
                    return JobOutcome::Failed(io_error("move the dump into place", e));
                }
                job.log(format!("Dump written to {}", target.display()));
                JobOutcome::Dumped {
                    path: target.to_path_buf(),
                }
            }
            Err(ToolEnd::Cancelled) => {
                remove_path(partial).await;
                JobOutcome::Cancelled
            }
            Err(ToolEnd::Failed(e)) => {
                remove_path(partial).await;
                JobOutcome::Failed(e)
            }
        }
    }

    /// Runs [`copy_sqlite`] on a blocking thread, repainting while it progresses.
    async fn sqlite_copy(
        &self,
        job: &JobControl,
        src: PathBuf,
        dst: PathBuf,
    ) -> Result<(), ToolEnd> {
        let worker = {
            let job = job.clone();
            tokio::task::spawn_blocking(move || copy_sqlite(&job, &src, &dst))
        };
        tokio::pin!(worker);
        let mut ticker = tokio::time::interval(CANCEL_TICK);
        loop {
            tokio::select! {
                done = &mut worker => {
                    self.repaint();
                    return done.unwrap_or_else(|e| {
                        Err(ToolEnd::Failed(config_error(format!("The backup stopped: {e}"))))
                    });
                }
                _ = ticker.tick() => self.repaint(),
            }
        }
    }

    /// Copies a SQLite database file over the connection's file with the online backup API.
    async fn sqlite_restore(
        &self,
        job: &JobControl,
        endpoint: &Endpoint,
        input: &Path,
    ) -> JobOutcome {
        let Some(db) = endpoint.conn.path.clone() else {
            return JobOutcome::Failed(config_error("The SQLite connection has no database file"));
        };
        if input == db {
            return JobOutcome::Failed(config_error(
                "The archive is the connection's own database file",
            ));
        }
        job.log(format!("Restoring {} (online backup API)", input.display()));
        match self.sqlite_copy(job, input.to_path_buf(), db).await {
            Ok(()) => {
                job.log("Restore complete");
                JobOutcome::Restored {
                    path: input.to_path_buf(),
                }
            }
            Err(ToolEnd::Cancelled) => JobOutcome::Cancelled,
            Err(ToolEnd::Failed(e)) => JobOutcome::Failed(e),
        }
    }

    /// The connection's values for its native tool, through the SSH tunnel when it has one.
    async fn tool_endpoint(&self, conn: &str, database: &str) -> Result<Endpoint, DriverError> {
        let entry = {
            let inner = self.inner.lock();
            inner.tool_conn(conn).ok_or_else(not_connected)?
        };
        let secrets = match entry.resolved.clone() {
            Some(secrets) => secrets,
            None => {
                self.connect_secrets(&entry.cfg, &entry.manifest, entry.given.clone())
                    .await
            }
        };
        let (via, tunnel) = self
            .tunnel_for(&entry.cfg, &secrets, &entry.manifest, &entry.tunnel)
            .await?;
        let manifest = &entry.manifest;
        let dialed_host = via
            .value(manifest, "host")
            .unwrap_or("localhost")
            .trim()
            .to_string();
        // Through a tunnel the tool dials 127.0.0.1 but TLS must still verify the host the
        // user named; libpq takes the two separately (`--host` and `--hostaddr`).
        let (host, host_addr) = if tunnel.is_some() && entry.cfg.driver == "postgres" {
            let named = entry
                .cfg
                .value(manifest, "host")
                .unwrap_or(&dialed_host)
                .trim()
                .to_string();
            (named, Some(dialed_host))
        } else {
            (dialed_host, None)
        };
        let port = via
            .value(manifest, "port")
            .and_then(|p| p.trim().parse().ok())
            .or(manifest.default_port)
            .unwrap_or(0);
        let field = |key: &str| {
            via.value(manifest, key)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let conn = ToolConnection {
            host,
            host_addr,
            port,
            user: field("user"),
            database: database.to_string(),
            password: secrets.0.get("password").cloned(),
            ssl_mode: field("ssl_mode"),
            ssl_ca: field("ssl_root_cert").or_else(|| field("ssl_ca")),
            ssl_cert: field("ssl_cert"),
            ssl_key: field("ssl_key"),
            path: field("path").map(PathBuf::from),
        };
        Ok(Endpoint {
            driver: entry.cfg.driver.clone(),
            conn,
            _tunnel: tunnel,
        })
    }

    /// Runs an external command, streaming its output into the job's log and killing it
    /// when the job is cancelled.
    async fn run_command(&self, job: &JobControl, command: &ToolCommand) -> Result<(), ToolEnd> {
        let program = resolve_program(&command.program)?;
        write_tool_files(command)?;
        let mut child = Command::new(&program);
        child
            .kill_on_drop(true)
            .args(&command.args)
            .stdin(if command.stdin_file.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .envs(command.env.iter().map(|(k, v)| (k, v)));
        // Its own process group, so a cancel takes the tool's children down with it.
        #[cfg(unix)]
        child.process_group(0);
        let mut child = child.spawn().map_err(|e| {
            ToolEnd::Failed(if e.kind() == io::ErrorKind::NotFound {
                missing_tool(&program)
            } else {
                io_error("start the tool", e)
            })
        })?;

        if let Some(path) = &command.stdin_file {
            let path = path.clone();
            let mut stdin = child.stdin.take().expect("piped stdin");
            let job = job.clone();
            tokio::spawn(async move {
                let result = async {
                    let mut file = tokio::fs::File::open(&path).await?;
                    tokio::io::copy(&mut file, &mut stdin).await?;
                    stdin.shutdown().await
                }
                .await;
                if let Err(e) = result {
                    job.log(format!("Could not feed {}: {e}", path.display()));
                }
            });
        }

        let (tx, mut rx) = mpsc::channel::<OutputLine>(LOG_CHANNEL);
        let bytes = Arc::new(AtomicU64::new(0));
        let mut stdout_task = None;
        if let Some(stdout) = child.stdout.take() {
            if let Some(target) = &command.stdout_file {
                if let Some(parent) = target.parent()
                    && !parent.as_os_str().is_empty()
                    && let Err(e) = tokio::fs::create_dir_all(parent).await
                {
                    let _ = child.kill().await;
                    return Err(ToolEnd::Failed(io_error(
                        "create the tool's output directory",
                        e,
                    )));
                }
                match tokio::fs::File::create(target).await {
                    Ok(mut file) => {
                        let bytes = bytes.clone();
                        stdout_task = Some(tokio::spawn(async move {
                            let mut buf = vec![0u8; 64 * 1024];
                            let mut reader = stdout;
                            loop {
                                let n = reader.read(&mut buf).await?;
                                if n == 0 {
                                    break;
                                }
                                file.write_all(&buf[..n]).await?;
                                bytes.fetch_add(n as u64, Ordering::Relaxed);
                            }
                            file.flush().await
                        }));
                    }
                    Err(e) => {
                        let _ = child.kill().await;
                        return Err(ToolEnd::Failed(io_error(
                            "create the tool's output file",
                            e,
                        )));
                    }
                }
            } else {
                let tx = tx.clone();
                tokio::spawn(async move {
                    read_lines(stdout, tx).await;
                });
            }
        }
        if let Some(stderr) = child.stderr.take() {
            let tx = tx.clone();
            tokio::spawn(async move {
                read_lines(stderr, tx).await;
            });
        }
        drop(tx);

        let mut ticker = tokio::time::interval(CANCEL_TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut lines = 0u64;
        loop {
            tokio::select! {
                status = child.wait() => {
                    let status = match status {
                        Ok(status) => status,
                        Err(e) => return Err(ToolEnd::Failed(io_error("wait for the tool", e))),
                    };
                    while let Some(line) = rx.recv().await {
                        lines += 1;
                        job.log(line.text);
                    }
                    if let Some(task) = stdout_task.take()
                        && let Ok(Err(e)) = task.await
                    {
                        return Err(ToolEnd::Failed(io_error(
                            "write the tool's output",
                            e,
                        )));
                    }
                    job.progress(JobProgress { rows: lines, bytes: bytes.load(Ordering::Relaxed) });
                    self.repaint();
                    return if status.success() {
                        Ok(())
                    } else {
                        let code = status
                            .code()
                            .map_or_else(|| "a signal".to_string(), |c| format!("code {c}"));
                        Err(ToolEnd::Failed(config_error(format!(
                            "The tool exited with {code}; see the log for its output"
                        ))))
                    };
                }
                Some(line) = rx.recv() => {
                    lines += 1;
                    job.log(line.text);
                    job.progress(JobProgress {
                        rows: lines,
                        bytes: bytes.load(Ordering::Relaxed),
                    });
                    self.repaint();
                }
                _ = ticker.tick() => {
                    if job.is_cancelled() {
                        kill_group(&mut child);
                        let _ = child.wait().await;
                        if let Some(task) = stdout_task.take() {
                            task.abort();
                        }
                        return Err(ToolEnd::Cancelled);
                    }
                    let written = if command.stdout_file.is_some() {
                        bytes.load(Ordering::Relaxed)
                    } else {
                        output_bytes(&command.args)
                    };
                    job.progress(JobProgress { rows: lines, bytes: written });
                    self.repaint();
                }
            }
        }
    }
}

/// Kills the tool and every process in its group.
fn kill_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: killpg only signals; the group id is the child's own (process_group(0)).
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
}

/// Pages copied per backup step; between steps the job's cancel flag is checked.
const BACKUP_PAGES_PER_STEP: std::os::raw::c_int = 128;

/// SQLite's online backup API: copies `src` into `dst`, reporting pages copied against the
/// total, stopping between steps when the job is cancelled. Readers and writers of either
/// file keep working; SQLite restarts the copy when a writer touches the source.
fn copy_sqlite(job: &JobControl, src: &Path, dst: &Path) -> Result<(), ToolEnd> {
    use rusqlite::backup::{Backup, StepResult};
    use rusqlite::{Connection, OpenFlags};
    let fail = |what: &str, e: rusqlite::Error| {
        ToolEnd::Failed(DriverError::new(ErrorKind::Other, format!("{what}: {e}")))
    };
    let source = Connection::open_with_flags(src, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| fail("Could not open the source database", e))?;
    let mut target =
        Connection::open(dst).map_err(|e| fail("Could not open the target database", e))?;
    let backup =
        Backup::new(&source, &mut target).map_err(|e| fail("Could not start the backup", e))?;
    loop {
        if job.is_cancelled() {
            return Err(ToolEnd::Cancelled);
        }
        let result = backup
            .step(BACKUP_PAGES_PER_STEP)
            .map_err(|e| fail("The backup failed", e))?;
        let progress = backup.progress();
        let total = progress.pagecount.max(0) as u64;
        job.set_total(total);
        job.progress(JobProgress {
            rows: total.saturating_sub(progress.remaining.max(0) as u64),
            bytes: 0,
        });
        match result {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            // The other side is busy: wait and retry.
            StepResult::Busy | StepResult::Locked => std::thread::sleep(CANCEL_TICK),
            _ => {}
        }
    }
}

/// How a child process ended, from [`Shared::run_command`]'s point of view.
enum ToolEnd {
    Cancelled,
    Failed(DriverError),
}

/// The bytes written so far, from a tool's `--result-file`/`--file` argument.
fn output_bytes(args: &[String]) -> u64 {
    for (i, arg) in args.iter().enumerate() {
        let path = if let Some(path) = arg.strip_prefix("--file=") {
            Some(path)
        } else if let Some(path) = arg.strip_prefix("--result-file=") {
            Some(path)
        } else if arg == "--file" || arg == "--result-file" {
            args.get(i + 1).map(String::as_str)
        } else if let Some(path) = arg
            .strip_prefix(".backup '")
            .and_then(|rest| rest.strip_suffix('\''))
        {
            return path_bytes(Path::new(&path.replace("''", "'")));
        } else {
            None
        };
        if let Some(path) = path {
            return path_bytes(Path::new(path));
        }
    }
    0
}

fn path_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| path_bytes(&entry.path()))
        .sum()
}

async fn read_lines<R>(reader: R, tx: mpsc::Sender<OutputLine>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = tokio::io::BufReader::new(reader).lines();
    while let Ok(Some(text)) = lines.next_line().await {
        if tx.send(OutputLine { text }).await.is_err() {
            break;
        }
    }
}

fn write_tool_files(command: &ToolCommand) -> Result<(), ToolEnd> {
    for file in &command.files {
        if let Some(parent) = file.path.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return Err(ToolEnd::Failed(io_error(
                "create the tool's temporary directory",
                e,
            )));
        }
        std::fs::write(&file.path, &file.content)
            .map_err(|e| ToolEnd::Failed(io_error("write the tool's temporary file", e)))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&file.path, std::fs::Permissions::from_mode(0o600));
        }
    }
    Ok(())
}

/// The tool binary: an explicit path is used as it is, a bare name is searched (PATH, then
/// the usual install locations).
fn resolve_program(program: &Path) -> Result<PathBuf, ToolEnd> {
    if program.components().count() > 1 || program.is_absolute() {
        return if program.is_file() {
            Ok(program.to_path_buf())
        } else {
            Err(ToolEnd::Failed(config_error(format!(
                "{} does not exist",
                program.display()
            ))))
        };
    }
    let name = program.to_string_lossy();
    find_tool(&name).ok_or_else(|| ToolEnd::Failed(missing_tool(program)))
}

/// Finds `name` on `PATH`, then in the usual PostgreSQL/MySQL/SQLite install locations.
pub(crate) fn find_tool(name: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Some(candidate) = in_dir(&dir, name) {
                return Some(candidate);
            }
        }
    }
    for dir in known_tool_dirs() {
        if let Some(candidate) = in_dir(&dir, name) {
            return Some(candidate);
        }
    }
    None
}

fn in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let candidate = dir.join(name);
    if candidate.is_file() {
        return Some(candidate);
    }
    #[cfg(windows)]
    for extension in ["exe", "cmd", "bat"] {
        let candidate = dir.join(format!("{name}.{extension}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn known_tool_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/opt/homebrew/opt/libpq/bin"),
        PathBuf::from("/opt/homebrew/opt/mysql-client/bin"),
        PathBuf::from("/usr/lib/postgresql"),
        PathBuf::from("/Applications/Postgres.app/Contents/Versions/latest/bin"),
    ];
    // Every installed Postgres.app version, newest first.
    let versions = Path::new("/Applications/Postgres.app/Contents/Versions");
    if let Ok(entries) = std::fs::read_dir(versions) {
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path().join("bin"))
            .filter(|path| path.is_dir())
            .collect();
        found.sort_by(|a, b| b.cmp(a));
        dirs.splice(0..0, found);
    }
    // PostgreSQL's versioned bin directories on Linux.
    if let Ok(entries) = std::fs::read_dir("/usr/lib/postgresql") {
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path().join("bin"))
            .filter(|path| path.is_dir())
            .collect();
        found.sort_by(|a, b| b.cmp(a));
        dirs.splice(0..0, found);
    }
    #[cfg(windows)]
    {
        for root in [r"C:\Program Files\PostgreSQL", r"C:\Program Files\MySQL"] {
            if let Ok(entries) = std::fs::read_dir(root) {
                dirs.extend(
                    entries
                        .flatten()
                        .map(|entry| entry.path().join("bin"))
                        .filter(|path| path.is_dir()),
                );
            }
        }
    }
    dirs
}

fn partial_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "dump".into());
    target.with_file_name(format!(".{name}.sqlr-partial"))
}

/// Removes a partial dump (a file, or the directory PostgreSQL's directory format made).
async fn remove_path(path: &Path) {
    let Ok(meta) = tokio::fs::symlink_metadata(path).await else {
        return;
    };
    if meta.is_dir() {
        let _ = tokio::fs::remove_dir_all(path).await;
    } else {
        let _ = tokio::fs::remove_file(path).await;
    }
}

/// Removes the target of a finished dump: only an empty directory for the directory
/// format, only a file for every other format. The pre-flight check refuses anything else.
async fn clear_target(target: &Path, directory: bool) -> io::Result<()> {
    let Ok(meta) = tokio::fs::symlink_metadata(target).await else {
        return Ok(());
    };
    if directory {
        if meta.is_dir() {
            tokio::fs::remove_dir(target).await?;
        }
    } else if !meta.is_dir() {
        tokio::fs::remove_file(target).await?;
    }
    Ok(())
}

fn config_error(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Other, message)
}

fn io_error(what: &str, e: io::Error) -> DriverError {
    DriverError::new(ErrorKind::Other, format!("Could not {what}: {e}"))
}

fn missing_tool(program: &Path) -> DriverError {
    let name = program
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.display().to_string());
    let hint = match name.as_str() {
        "pg_dump" | "pg_restore" | "psql" => {
            "Install the PostgreSQL client tools (macOS: brew install libpq or Postgres.app; \
             Debian/Ubuntu: apt install postgresql-client) or give the executable's path in \
             the dialog."
        }
        "mysqldump" | "mysql" => {
            "Install the MySQL client tools (macOS: brew install mysql-client; \
             Debian/Ubuntu: apt install mysql-client) or give the executable's path in the \
             dialog."
        }
        "sqlite3" => "Install the sqlite3 command-line tool or give its path in the dialog.",
        _ => "Install the tool or give its path in the dialog.",
    };
    DriverError::new(ErrorKind::Other, format!("{name} was not found. {hint}"))
}
