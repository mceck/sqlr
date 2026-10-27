//! Native dump and restore (spec §8.5): the command lines as data.
//!
//! The engine discovers the tools, runs the child processes and streams their output; this
//! module only decides what to run. Passwords never enter a command line: PostgreSQL tools
//! get theirs in the child's environment, MySQL tools in a temporary defaults file, and
//! [`ToolCommand::display`] never prints either.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::node::{Node, NodeKind};

/// Whether a driver's databases can be dumped and restored with native tools.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DatabaseTools {
    /// Whether a database can be dumped.
    pub dump: bool,
    /// Whether a database can be restored.
    pub restore: bool,
}

impl DatabaseTools {
    /// A driver whose databases can be both dumped and restored.
    pub fn both() -> Self {
        Self {
            dump: true,
            restore: true,
        }
    }
}

/// The explorer nodes that stand for a whole database: a database node, or SQLite's `main`
/// schema (its file is the database).
pub fn is_database_node(driver: &str, node: &Node) -> bool {
    match node.kind {
        NodeKind::Database => true,
        NodeKind::Schema => {
            driver == "sqlite" && node.path.segments().len() == 1 && node.name == "main"
        }
        _ => false,
    }
}

/// The format a dump is written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DumpFormat {
    /// PostgreSQL `pg_dump -Fp`: a plain SQL file.
    Plain,
    /// PostgreSQL `pg_dump -Fc`.
    Custom,
    /// PostgreSQL `pg_dump -Fd`.
    Directory,
    /// PostgreSQL `pg_dump -Ft`.
    Tar,
    /// MySQL `mysqldump`: a plain SQL file.
    MysqlSql,
    /// SQLite's `.backup`: a copy of the database file.
    SqliteBinary,
}

impl DumpFormat {
    /// The file extension of a dump in this format.
    pub fn extension(self) -> &'static str {
        match self {
            DumpFormat::Plain | DumpFormat::MysqlSql => "sql",
            DumpFormat::Custom => "dump",
            DumpFormat::Tar => "tar",
            DumpFormat::Directory => "dir",
            DumpFormat::SqliteBinary => "db",
        }
    }
}

/// What a dump writes and which tool options it passes.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DumpOptions {
    /// The dump format.
    pub format: DumpFormat,
    /// Dump the structure only (`--schema-only`).
    pub schema_only: bool,
    /// Dump the data only (`--data-only`).
    pub data_only: bool,
    /// Add statements that drop objects before creating them (`--clean`).
    pub clean: bool,
    /// With `clean`: use `IF EXISTS` on the drops.
    pub if_exists: bool,
    /// Add the statement that creates the database (`--create`).
    pub create: bool,
    /// Parallel jobs: PostgreSQL directory format only.
    pub jobs: Option<u8>,
    /// PostgreSQL custom/directory compression level (`-Z`), 0–9.
    pub compression: Option<u8>,
    /// Dump encoding (`--encoding`).
    pub encoding: Option<String>,
    /// Leave out ownership statements (`--no-owner`).
    pub no_owner: bool,
    /// Leave out privilege statements (`--no-privileges`).
    pub no_privileges: bool,
    /// MySQL: include stored routines, events and triggers.
    pub mysql_routines: bool,
    /// MySQL: include events.
    pub mysql_events: bool,
    /// MySQL: include triggers.
    pub mysql_triggers: bool,
    /// MySQL: `--single-transaction` (InnoDB's consistent snapshot).
    pub mysql_single_transaction: bool,
    /// Extra command-line arguments, split on whitespace with quotes honoured.
    pub extra_args: String,
    /// An explicit tool binary; empty searches `PATH` and the usual install locations.
    pub executable: String,
}

impl Default for DumpOptions {
    fn default() -> Self {
        Self {
            format: DumpFormat::Plain,
            schema_only: false,
            data_only: false,
            clean: false,
            if_exists: false,
            create: false,
            jobs: None,
            compression: None,
            encoding: None,
            no_owner: false,
            no_privileges: false,
            mysql_routines: true,
            mysql_events: true,
            mysql_triggers: true,
            mysql_single_transaction: true,
            extra_args: String::new(),
            executable: String::new(),
        }
    }
}

/// How a restore reads an archive and what it does to the target.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RestoreOptions {
    /// Drop objects before recreating them (`--clean`).
    pub clean: bool,
    /// With `clean`: use `IF EXISTS` on the drops.
    pub if_exists: bool,
    /// Create the database before restoring (`--create`).
    pub create: bool,
    /// Parallel jobs.
    pub jobs: Option<u8>,
    /// Restore in a single transaction.
    pub single_transaction: bool,
    /// Restore the structure only.
    pub schema_only: bool,
    /// Restore the data only.
    pub data_only: bool,
    /// Skip ownership statements.
    pub no_owner: bool,
    /// Skip privilege statements.
    pub no_privileges: bool,
    /// Stop at the first error.
    pub exit_on_error: bool,
    /// Extra command-line arguments, split on whitespace with quotes honoured.
    pub extra_args: String,
    /// An explicit tool binary; empty searches `PATH` and the usual install locations.
    pub executable: String,
}

impl Default for RestoreOptions {
    fn default() -> Self {
        Self {
            clean: false,
            if_exists: false,
            create: false,
            jobs: None,
            single_transaction: true,
            schema_only: false,
            data_only: false,
            no_owner: false,
            no_privileges: false,
            exit_on_error: true,
            extra_args: String::new(),
            executable: String::new(),
        }
    }
}

/// The parameters a native tool connects with. `password` is never rendered or printed.
#[derive(Clone, Default, PartialEq)]
pub struct ToolConnection {
    /// The host TLS verifies (the one the user named); with an SSH tunnel the tool dials
    /// [`Self::host_addr`] instead.
    pub host: String,
    /// The address to dial when it differs from `host` (a tunnel's `127.0.0.1`).
    pub host_addr: Option<String>,
    /// Server port.
    pub port: u16,
    /// User name.
    pub user: Option<String>,
    /// Database to dump or restore.
    pub database: String,
    /// Password; handed over through the environment or a defaults file, never an argument.
    pub password: Option<String>,
    /// PostgreSQL `PGSSLMODE` values (`disable`, `require`, `verify-full`, …).
    pub ssl_mode: Option<String>,
    /// CA certificate file for TLS.
    pub ssl_ca: Option<String>,
    /// Client certificate file for TLS.
    pub ssl_cert: Option<String>,
    /// Client key file for TLS.
    pub ssl_key: Option<String>,
    /// SQLite's database file.
    pub path: Option<PathBuf>,
}

impl fmt::Debug for ToolConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolConnection")
            .field("host", &self.host)
            .field("host_addr", &self.host_addr)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("database", &self.database)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("ssl_mode", &self.ssl_mode)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// A file a tool needs before it starts (MySQL's temporary defaults file), written by the
/// engine with owner-only permissions and removed when the tool ends.
#[derive(Clone, PartialEq)]
pub struct ToolFile {
    /// Where the file is written.
    pub path: PathBuf,
    /// The file's contents.
    pub content: String,
}

impl fmt::Debug for ToolFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolFile")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// A ready-to-run child process. `env` may hold secrets; `Debug` and [`Self::display`] keep
/// their values out of logs.
#[derive(Clone, PartialEq)]
pub struct ToolCommand {
    /// The executable to run.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// Environment variables of the child, which may hold secrets.
    pub env: Vec<(String, String)>,
    /// Files to write before it starts.
    pub files: Vec<ToolFile>,
    /// A file piped to the tool's standard input (a restore).
    pub stdin_file: Option<PathBuf>,
    /// Where the tool's standard output goes (SQLite's `.dump`).
    pub stdout_file: Option<PathBuf>,
}

impl fmt::Debug for ToolCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolCommand")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("env", &self.env.iter().map(|(k, _)| k).collect::<Vec<_>>())
            .field("files", &self.files)
            .field("stdin_file", &self.stdin_file)
            .field("stdout_file", &self.stdout_file)
            .finish()
    }
}

impl ToolCommand {
    /// The command line to show the user: no environment values and no file contents.
    pub fn display(&self) -> String {
        let mut out = self.program.display().to_string();
        for arg in &self.args {
            out.push(' ');
            out.push_str(&shell_quote(arg));
        }
        out
    }
}

/// Quotes an argument for display, following POSIX shell rules closely enough for a
/// readable command line.
pub fn shell_quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

/// Splits the dialog's "extra arguments" field: whitespace separates, single and double
/// quotes group, and a backslash escapes the next character outside single quotes.
pub fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for c in text.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            started = true;
            continue;
        }
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            Some(_) => {
                if c == '"' {
                    quote = None;
                } else if c == '\\' {
                    escaped = true;
                } else {
                    current.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                '\\' => {
                    escaped = true;
                    started = true;
                }
                c if c.is_whitespace() => {
                    if started || !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                c => {
                    current.push(c);
                    started = true;
                }
            },
        }
    }
    if started || !current.is_empty() {
        args.push(current);
    }
    args
}

fn tool_program(default: &str, explicit: &str) -> PathBuf {
    let explicit = explicit.trim();
    if explicit.is_empty() {
        PathBuf::from(default)
    } else {
        PathBuf::from(explicit)
    }
}

fn tool_env(conn: &ToolConnection) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(password) = &conn.password
        && !password.is_empty()
    {
        env.push(("PGPASSWORD".into(), password.clone()));
    }
    if let Some(mode) = &conn.ssl_mode
        && !mode.is_empty()
    {
        env.push(("PGSSLMODE".into(), mode.clone()));
    }
    if let Some(path) = &conn.ssl_ca
        && !path.is_empty()
    {
        env.push(("PGSSLROOTCERT".into(), path.clone()));
    }
    if let Some(path) = &conn.ssl_cert
        && !path.is_empty()
    {
        env.push(("PGSSLCERT".into(), path.clone()));
    }
    if let Some(path) = &conn.ssl_key
        && !path.is_empty()
    {
        env.push(("PGSSLKEY".into(), path.clone()));
    }
    env
}

fn pg_connection_args(conn: &ToolConnection, database: &str) -> Vec<String> {
    // A missing password must fail, not wait for a prompt on a terminal the app does not
    // have; a `.pgpass` file still works.
    let mut args = vec!["--no-password".into(), "--host".into(), conn.host.clone()];
    if let Some(addr) = &conn.host_addr
        && !addr.is_empty()
    {
        args.push("--hostaddr".into());
        args.push(addr.clone());
    }
    if conn.port != 0 {
        args.push("--port".into());
        args.push(conn.port.to_string());
    }
    if let Some(user) = &conn.user
        && !user.is_empty()
    {
        args.push("--username".into());
        args.push(user.clone());
    }
    args.push("--dbname".into());
    // libpq expands names containing '=' or a URI scheme as connection strings. Wrap
    // every name in a quoted dbname value so catalog names cannot redirect credentials.
    let database = database.replace('\\', "\\\\").replace('\'', "\\'");
    args.push(format!("dbname='{database}'"));
    args
}

fn validate_exclusive(opts: &DumpOptions) -> Result<(), String> {
    if opts.schema_only && opts.data_only {
        return Err("Schema only and data only cannot both be selected".into());
    }
    Ok(())
}

fn validate_restore_exclusive(opts: &RestoreOptions) -> Result<(), String> {
    if opts.schema_only && opts.data_only {
        return Err("Schema only and data only cannot both be selected".into());
    }
    Ok(())
}

fn jobs(value: Option<u8>) -> Option<u8> {
    value.map(|n| n.clamp(1, 64))
}

/// The command that dumps `conn`'s `database` (SQLite: the whole file) to `out`.
///
/// `temp_dir` holds files a tool needs while it runs; the engine owns it.
pub fn dump_command(
    driver: &str,
    conn: &ToolConnection,
    opts: &DumpOptions,
    out: &Path,
    temp_dir: &Path,
) -> Result<ToolCommand, String> {
    validate_exclusive(opts)?;
    match driver {
        "postgres" => Ok(pg_dump_command(conn, opts, out)),
        "mysql" => Ok(mysql_dump_command(conn, opts, out, temp_dir)),
        "sqlite" => sqlite_dump_command(conn, opts, out),
        other => Err(format!("{other} databases cannot be dumped")),
    }
}

/// The command that restores `input` into `conn`'s `database` (SQLite: the whole file).
pub fn restore_command(
    driver: &str,
    conn: &ToolConnection,
    opts: &RestoreOptions,
    input: &Path,
    temp_dir: &Path,
) -> Result<ToolCommand, String> {
    validate_restore_exclusive(opts)?;
    match driver {
        "postgres" => Ok(pg_restore_command(conn, opts, input)),
        "mysql" => Ok(mysql_restore_command(conn, opts, input, temp_dir)),
        "sqlite" => sqlite_restore_command(conn, opts, input),
        other => Err(format!("{other} databases cannot be restored")),
    }
}

fn pg_dump_command(conn: &ToolConnection, opts: &DumpOptions, out: &Path) -> ToolCommand {
    let mut args = pg_connection_args(conn, &conn.database);
    let format = match opts.format {
        DumpFormat::Plain => Some("plain"),
        DumpFormat::Custom => Some("custom"),
        DumpFormat::Directory => Some("directory"),
        DumpFormat::Tar => Some("tar"),
        _ => None,
    };
    if let Some(format) = format {
        args.push(format!("--format={format}"));
    }
    args.push("--file".into());
    args.push(out.display().to_string());
    if opts.schema_only {
        args.push("--schema-only".into());
    }
    if opts.data_only {
        args.push("--data-only".into());
    }
    if opts.clean {
        args.push("--clean".into());
        if opts.if_exists {
            args.push("--if-exists".into());
        }
    }
    if opts.create {
        args.push("--create".into());
    }
    if opts.no_owner {
        args.push("--no-owner".into());
    }
    if opts.no_privileges {
        args.push("--no-privileges".into());
    }
    if let Some(level) = opts.compression
        && matches!(opts.format, DumpFormat::Custom | DumpFormat::Directory)
    {
        args.push(format!("--compress={}", level.min(9)));
    }
    if opts.format == DumpFormat::Directory
        && let Some(n) = jobs(opts.jobs)
    {
        args.push(format!("--jobs={n}"));
    }
    if let Some(encoding) = &opts.encoding
        && !encoding.trim().is_empty()
    {
        args.push(format!("--encoding={}", encoding.trim()));
    }
    args.push("--verbose".into());
    args.extend(split_args(&opts.extra_args));
    ToolCommand {
        program: tool_program("pg_dump", &opts.executable),
        args,
        env: tool_env(conn),
        files: Vec::new(),
        stdin_file: None,
        stdout_file: None,
    }
}

fn pg_restore_command(conn: &ToolConnection, opts: &RestoreOptions, input: &Path) -> ToolCommand {
    if input.is_dir() || !is_sql_file(input) {
        let mut args = pg_connection_args(
            conn,
            if opts.create {
                "postgres"
            } else {
                &conn.database
            },
        );
        args.push("--verbose".into());
        if opts.exit_on_error {
            args.push("--exit-on-error".into());
        }
        if opts.clean {
            args.push("--clean".into());
            if opts.if_exists {
                args.push("--if-exists".into());
            }
        }
        if opts.create {
            args.push("--create".into());
        }
        if opts.no_owner {
            args.push("--no-owner".into());
        }
        if opts.no_privileges {
            args.push("--no-privileges".into());
        }
        if opts.schema_only {
            args.push("--schema-only".into());
        }
        if opts.data_only {
            args.push("--data-only".into());
        }
        // `CREATE DATABASE` of `--create` cannot run inside a transaction.
        if opts.single_transaction && !opts.create {
            args.push("--single-transaction".into());
        } else if !opts.create
            && let Some(n) = jobs(opts.jobs)
        {
            args.push(format!("--jobs={n}"));
        }
        args.extend(split_args(&opts.extra_args));
        args.push("--".into());
        args.push(input.display().to_string());
        return ToolCommand {
            program: tool_program("pg_restore", &opts.executable),
            args,
            env: tool_env(conn),
            files: Vec::new(),
            stdin_file: None,
            stdout_file: None,
        };
    }
    let mut args = pg_connection_args(conn, &conn.database);
    if opts.exit_on_error {
        args.push("--set=ON_ERROR_STOP=on".into());
    }
    if opts.single_transaction {
        args.push("--single-transaction".into());
    }
    args.extend(split_args(&opts.extra_args));
    args.push("--file".into());
    args.push(input.display().to_string());
    ToolCommand {
        program: tool_program("psql", &opts.executable),
        args,
        env: tool_env(conn),
        files: Vec::new(),
        stdin_file: None,
        stdout_file: None,
    }
}

/// Whether a restore input is a SQL script (run by a client) rather than a binary archive.
pub fn is_sql_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
}

fn mysql_defaults_file(conn: &ToolConnection, temp_dir: &Path) -> ToolFile {
    let mut content = String::from("[client]\n");
    if !conn.host.is_empty() {
        content.push_str(&format!("host={}\n", mysql_option_value(&conn.host)));
    }
    if conn.port != 0 {
        content.push_str(&format!("port={}\n", conn.port));
    }
    if let Some(user) = &conn.user
        && !user.is_empty()
    {
        content.push_str(&format!("user={}\n", mysql_option_value(user)));
    }
    if let Some(password) = &conn.password
        && !password.is_empty()
    {
        content.push_str(&format!("password={}\n", mysql_option_value(password)));
    }
    ToolFile {
        path: temp_dir.join("sqlr-client.cnf"),
        content,
    }
}

/// Quotes one value for MySQL's line-oriented option parser, including control
/// characters so a connection value cannot introduce another option or group.
fn mysql_option_value(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace('\u{8}', "\\b");
    format!("\"{escaped}\"")
}

fn mysql_ssl_args(conn: &ToolConnection, args: &mut Vec<String>) {
    if let Some(mode) = &conn.ssl_mode
        && !mode.is_empty()
    {
        let mode = match mode.as_str() {
            "disabled" => "DISABLED",
            "preferred" => "PREFERRED",
            "required" => "REQUIRED",
            "verify_ca" => "VERIFY_CA",
            "verify_identity" => "VERIFY_IDENTITY",
            other => other,
        };
        args.push(format!("--ssl-mode={mode}"));
    }
    if let Some(path) = &conn.ssl_ca
        && !path.is_empty()
    {
        args.push(format!("--ssl-ca={path}"));
    }
    if let Some(path) = &conn.ssl_cert
        && !path.is_empty()
    {
        args.push(format!("--ssl-cert={path}"));
    }
    if let Some(path) = &conn.ssl_key
        && !path.is_empty()
    {
        args.push(format!("--ssl-key={path}"));
    }
}

fn mysql_dump_command(
    conn: &ToolConnection,
    opts: &DumpOptions,
    out: &Path,
    temp_dir: &Path,
) -> ToolCommand {
    let defaults = mysql_defaults_file(conn, temp_dir);
    let mut args = vec![
        format!("--defaults-extra-file={}", defaults.path.display()),
        "--protocol=TCP".into(),
        "--quick".into(),
        "--verbose".into(),
        format!("--result-file={}", out.display()),
    ];
    if opts.mysql_single_transaction {
        args.push("--single-transaction".into());
    }
    if opts.mysql_routines {
        args.push("--routines".into());
    }
    if opts.mysql_events {
        args.push("--events".into());
    }
    if opts.mysql_triggers {
        args.push("--triggers".into());
    }
    if !conn.host.is_empty() {
        args.push(format!("--host={}", conn.host));
    }
    if conn.port != 0 {
        args.push(format!("--port={}", conn.port));
    }
    if let Some(user) = &conn.user
        && !user.is_empty()
    {
        args.push(format!("--user={user}"));
    }
    if opts.schema_only {
        args.push("--no-data".into());
    }
    if opts.data_only {
        args.push("--no-create-info".into());
    }
    if opts.clean {
        args.push("--add-drop-table".into());
        if opts.create {
            args.push("--add-drop-database".into());
        }
    }
    if opts.create {
        args.push("--databases".into());
    }
    mysql_ssl_args(conn, &mut args);
    if let Some(encoding) = &opts.encoding
        && !encoding.trim().is_empty()
    {
        args.push(format!("--default-character-set={}", encoding.trim()));
    }
    args.extend(split_args(&opts.extra_args));
    args.push("--".into());
    args.push(conn.database.clone());
    ToolCommand {
        program: tool_program("mysqldump", &opts.executable),
        args,
        env: Vec::new(),
        files: vec![defaults],
        stdin_file: None,
        stdout_file: None,
    }
}

fn mysql_restore_command(
    conn: &ToolConnection,
    opts: &RestoreOptions,
    input: &Path,
    temp_dir: &Path,
) -> ToolCommand {
    let defaults = mysql_defaults_file(conn, temp_dir);
    let mut args = vec![
        format!("--defaults-extra-file={}", defaults.path.display()),
        "--protocol=TCP".into(),
    ];
    if !conn.host.is_empty() {
        args.push(format!("--host={}", conn.host));
    }
    if conn.port != 0 {
        args.push(format!("--port={}", conn.port));
    }
    if let Some(user) = &conn.user
        && !user.is_empty()
    {
        args.push(format!("--user={user}"));
    }
    if !conn.database.is_empty() && !opts.create {
        args.push(format!("--database={}", conn.database));
    }
    mysql_ssl_args(conn, &mut args);
    args.extend(split_args(&opts.extra_args));
    ToolCommand {
        program: tool_program("mysql", &opts.executable),
        args,
        env: Vec::new(),
        files: vec![defaults],
        stdin_file: Some(input.to_path_buf()),
        stdout_file: None,
    }
}

fn sqlite_path(conn: &ToolConnection) -> Result<&Path, String> {
    conn.path
        .as_deref()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "The SQLite connection has no database file".to_string())
}

fn sqlite_dump_command(
    conn: &ToolConnection,
    opts: &DumpOptions,
    out: &Path,
) -> Result<ToolCommand, String> {
    let db = sqlite_path(conn)?;
    if opts.data_only {
        return Err("SQLite cannot dump data without the schema".into());
    }
    let mut args = vec![db.display().to_string()];
    let stdout_file = match opts.format {
        DumpFormat::SqliteBinary => {
            args.push(format!(".backup {}", sqlite_string(out)));
            None
        }
        _ if opts.schema_only => {
            args.push(".schema".into());
            Some(out.to_path_buf())
        }
        _ => {
            args.push(".dump".into());
            Some(out.to_path_buf())
        }
    };
    Ok(ToolCommand {
        program: tool_program("sqlite3", &opts.executable),
        args,
        env: Vec::new(),
        files: Vec::new(),
        stdin_file: None,
        stdout_file,
    })
}

fn sqlite_restore_command(
    conn: &ToolConnection,
    opts: &RestoreOptions,
    input: &Path,
) -> Result<ToolCommand, String> {
    let db = sqlite_path(conn)?;
    if input == db {
        return Err("The archive is the connection's own database file".into());
    }
    if is_sql_file(input) {
        return Ok(ToolCommand {
            program: tool_program("sqlite3", &opts.executable),
            args: vec![
                db.display().to_string(),
                ".bail on".into(),
                format!(".read {}", sqlite_string(input)),
            ],
            env: Vec::new(),
            files: Vec::new(),
            stdin_file: None,
            stdout_file: None,
        });
    }
    Ok(ToolCommand {
        program: tool_program("sqlite3", &opts.executable),
        args: vec![
            input.display().to_string(),
            format!(".backup {}", sqlite_string(db)),
        ],
        env: Vec::new(),
        files: Vec::new(),
        stdin_file: None,
        stdout_file: None,
    })
}

/// A SQLite dot-command argument: a single-quoted SQL literal.
fn sqlite_string(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg() -> ToolConnection {
        ToolConnection {
            host: "db.example".into(),
            port: 5432,
            user: Some("alice".into()),
            database: "app".into(),
            password: Some("hunter2".into()),
            ssl_mode: Some("require".into()),
            ..Default::default()
        }
    }

    #[test]
    fn security_postgres_database_names_cannot_override_connection_parameters() {
        for (database, expected) in [
            (
                "host=attacker.invalid sslmode=disable",
                "dbname='host=attacker.invalid sslmode=disable'",
            ),
            (
                "postgresql://attacker.invalid/app",
                "dbname='postgresql://attacker.invalid/app'",
            ),
            (
                "app' host=attacker.invalid",
                "dbname='app\\' host=attacker.invalid'",
            ),
            (
                "app\\' host=attacker.invalid",
                "dbname='app\\\\\\' host=attacker.invalid'",
            ),
            ("app\nsslmode=disable", "dbname='app\nsslmode=disable'"),
        ] {
            let conn = ToolConnection {
                database: database.into(),
                ..pg()
            };
            let commands = [
                dump_command(
                    "postgres",
                    &conn,
                    &DumpOptions::default(),
                    Path::new("/tmp/out.sql"),
                    Path::new("/tmp"),
                )
                .unwrap(),
                restore_command(
                    "postgres",
                    &conn,
                    &RestoreOptions::default(),
                    Path::new("/tmp/in.sql"),
                    Path::new("/tmp"),
                )
                .unwrap(),
                restore_command(
                    "postgres",
                    &conn,
                    &RestoreOptions::default(),
                    Path::new("/tmp/in.dump"),
                    Path::new("/tmp"),
                )
                .unwrap(),
            ];
            for command in commands {
                let argument = command
                    .args
                    .windows(2)
                    .find(|pair| pair[0] == "--dbname")
                    .unwrap();
                assert_eq!(
                    argument[1],
                    expected,
                    "{} must keep the database name literal",
                    command.program.display()
                );
            }
        }
    }

    #[test]
    fn security_postgres_restore_archive_cannot_be_an_option() {
        let cmd = restore_command(
            "postgres",
            &pg(),
            &RestoreOptions::default(),
            Path::new("--file=unexpected.dump"),
            Path::new("/tmp"),
        )
        .unwrap();
        assert_eq!(
            &cmd.args[cmd.args.len() - 2..],
            ["--", "--file=unexpected.dump"]
        );
    }

    #[test]
    fn security_mysql_defaults_cannot_inject_options_through_line_breaks() {
        let conn = ToolConnection {
            host: "db.example\r\n[client]\nhost=attacker.invalid\n#".into(),
            user: Some("alice\n[mysqldump]\nwhere=0\n#".into()),
            password: Some("p\\\"\t\u{8}\r\n[mysql]\nexecute=SELECT 1\n#".into()),
            ..mysql()
        };
        let commands = [
            dump_command(
                "mysql",
                &conn,
                &DumpOptions::default(),
                Path::new("/tmp/out.sql"),
                Path::new("/tmp"),
            )
            .unwrap(),
            restore_command(
                "mysql",
                &conn,
                &RestoreOptions::default(),
                Path::new("/tmp/in.sql"),
                Path::new("/tmp"),
            )
            .unwrap(),
        ];
        for command in commands {
            assert_eq!(
                command.files[0].content,
                concat!(
                    "[client]\n",
                    "host=\"db.example\\r\\n[client]\\nhost=attacker.invalid\\n#\"\n",
                    "port=3306\n",
                    "user=\"alice\\n[mysqldump]\\nwhere=0\\n#\"\n",
                    "password=\"p\\\\\\\"\\t\\b\\r\\n[mysql]\\nexecute=SELECT 1\\n#\"\n",
                )
            );
        }
    }

    #[test]
    fn security_mysql_dump_database_cannot_be_an_option() {
        for create in [false, true] {
            let conn = ToolConnection {
                database: "--all-databases".into(),
                ..mysql()
            };
            let opts = DumpOptions {
                create,
                extra_args: "--hex-blob".into(),
                ..Default::default()
            };
            let cmd = dump_command(
                "mysql",
                &conn,
                &opts,
                Path::new("/tmp/out.sql"),
                Path::new("/tmp"),
            )
            .unwrap();
            assert_eq!(
                &cmd.args[cmd.args.len() - 3..],
                ["--hex-blob", "--", "--all-databases"]
            );
        }
    }

    fn mysql() -> ToolConnection {
        ToolConnection {
            host: "db.example".into(),
            port: 3306,
            user: Some("root".into()),
            database: "app".into(),
            password: Some("p@ss word".into()),
            ..Default::default()
        }
    }

    fn sqlite() -> ToolConnection {
        ToolConnection {
            path: Some(PathBuf::from("/data/app.db")),
            database: "main".into(),
            ..Default::default()
        }
    }

    #[test]
    fn pg_dump_plain_has_connection_and_file_and_hides_the_password() {
        let out = Path::new("/tmp/app.sql");
        let cmd = dump_command(
            "postgres",
            &pg(),
            &DumpOptions::default(),
            out,
            Path::new("/tmp"),
        )
        .unwrap();
        assert_eq!(cmd.program, PathBuf::from("pg_dump"));
        for arg in [
            "--host",
            "db.example",
            "--port",
            "5432",
            "--username",
            "alice",
            "--dbname",
            "dbname='app'",
            "--format=plain",
            "--file",
            "/tmp/app.sql",
            "--verbose",
        ] {
            assert!(
                cmd.args.iter().any(|a| a == arg),
                "missing {arg}: {:?}",
                cmd.args
            );
        }
        assert_eq!(
            cmd.env,
            vec![
                ("PGPASSWORD".into(), "hunter2".into()),
                ("PGSSLMODE".into(), "require".into())
            ]
        );
        assert!(!cmd.display().contains("hunter2"));
        assert!(!format!("{cmd:?}").contains("hunter2"));
    }

    #[test]
    fn pg_tools_dial_a_tunnel_but_verify_the_named_host() {
        let conn = ToolConnection {
            host_addr: Some("127.0.0.1".into()),
            ..pg()
        };
        let cmd = dump_command(
            "postgres",
            &conn,
            &DumpOptions::default(),
            Path::new("/tmp/app.sql"),
            Path::new("/tmp"),
        )
        .unwrap();
        assert!(cmd.args.windows(2).any(|w| w == ["--host", "db.example"]));
        assert!(
            cmd.args
                .windows(2)
                .any(|w| w == ["--hostaddr", "127.0.0.1"])
        );
    }

    #[test]
    fn pg_dump_custom_passes_options() {
        let opts = DumpOptions {
            format: DumpFormat::Custom,
            schema_only: true,
            clean: true,
            if_exists: true,
            create: true,
            compression: Some(6),
            no_owner: true,
            no_privileges: true,
            encoding: Some("UTF8".into()),
            ..Default::default()
        };
        let cmd = dump_command(
            "postgres",
            &pg(),
            &opts,
            Path::new("/tmp/app.dump"),
            Path::new("/tmp"),
        )
        .unwrap();
        for arg in [
            "--format=custom",
            "--schema-only",
            "--clean",
            "--if-exists",
            "--create",
            "--compress=6",
            "--no-owner",
            "--no-privileges",
            "--encoding=UTF8",
        ] {
            assert!(cmd.args.iter().any(|a| a == arg), "missing {arg}");
        }
    }

    #[test]
    fn pg_dump_refuses_schema_and_data_only_together() {
        let opts = DumpOptions {
            schema_only: true,
            data_only: true,
            ..Default::default()
        };
        assert!(
            dump_command(
                "postgres",
                &pg(),
                &opts,
                Path::new("/tmp/app.sql"),
                Path::new("/tmp")
            )
            .is_err()
        );
    }

    #[test]
    fn pg_restore_custom_archive_uses_pg_restore() {
        let opts = RestoreOptions {
            clean: true,
            if_exists: true,
            no_owner: true,
            single_transaction: false,
            jobs: Some(4),
            ..Default::default()
        };
        let cmd = restore_command(
            "postgres",
            &pg(),
            &opts,
            Path::new("/tmp/app.dump"),
            Path::new("/tmp"),
        )
        .unwrap();
        assert_eq!(cmd.program, PathBuf::from("pg_restore"));
        for arg in [
            "--clean",
            "--if-exists",
            "--no-owner",
            "--jobs=4",
            "--exit-on-error",
            "/tmp/app.dump",
        ] {
            assert!(cmd.args.iter().any(|a| a == arg), "missing {arg}");
        }
    }

    #[test]
    fn pg_restore_create_database_never_uses_a_single_transaction() {
        let opts = RestoreOptions {
            create: true,
            single_transaction: true,
            ..Default::default()
        };
        let cmd = restore_command(
            "postgres",
            &pg(),
            &opts,
            Path::new("/tmp/app.dump"),
            Path::new("/tmp"),
        )
        .unwrap();
        assert!(cmd.args.iter().any(|a| a == "--create"));
        assert!(!cmd.args.iter().any(|a| a == "--single-transaction"));
        assert!(
            cmd.args
                .windows(2)
                .any(|w| w == ["--dbname", "dbname='postgres'"])
        );
    }

    #[test]
    fn pg_restore_plain_sql_uses_psql() {
        let cmd = restore_command(
            "postgres",
            &pg(),
            &RestoreOptions::default(),
            Path::new("/tmp/app.sql"),
            Path::new("/tmp"),
        )
        .unwrap();
        assert_eq!(cmd.program, PathBuf::from("psql"));
        assert!(cmd.args.iter().any(|a| a == "--set=ON_ERROR_STOP=on"));
        assert!(cmd.args.iter().any(|a| a == "--single-transaction"));
        assert!(cmd.args.iter().any(|a| a == "/tmp/app.sql"));
        assert!(!cmd.display().contains("hunter2"));
    }

    #[test]
    fn mysql_dump_uses_a_defaults_file_and_never_the_command_line() {
        let temp = tempfile::tempdir().unwrap();
        let opts = DumpOptions {
            format: DumpFormat::MysqlSql,
            ..Default::default()
        };
        let cmd = dump_command(
            "mysql",
            &mysql(),
            &opts,
            Path::new("/tmp/app.sql"),
            temp.path(),
        )
        .unwrap();
        assert_eq!(cmd.program, PathBuf::from("mysqldump"));
        let defaults = cmd.files.first().expect("a defaults file");
        assert!(defaults.content.contains("password=\"p@ss word\""));
        assert!(
            cmd.args
                .iter()
                .any(|a| a == &format!("--defaults-extra-file={}", defaults.path.display()))
        );
        assert!(!cmd.args.iter().any(|a| a == "--no-create-info"));
        assert!(!cmd.display().contains("p@ss word"));
        assert!(!format!("{cmd:?}").contains("p@ss word"));
    }

    #[test]
    fn mysql_dump_options_are_translated() {
        let temp = tempfile::tempdir().unwrap();
        let opts = DumpOptions {
            format: DumpFormat::MysqlSql,
            schema_only: true,
            clean: true,
            create: true,
            encoding: Some("utf8mb4".into()),
            ..Default::default()
        };
        let cmd = dump_command(
            "mysql",
            &mysql(),
            &opts,
            Path::new("/tmp/app.sql"),
            temp.path(),
        )
        .unwrap();
        for arg in [
            "--no-data",
            "--add-drop-table",
            "--add-drop-database",
            "--databases",
            "--default-character-set=utf8mb4",
        ] {
            assert!(cmd.args.iter().any(|a| a == arg), "missing {arg}");
        }
    }

    #[test]
    fn mysql_dump_can_leave_routines_and_the_snapshot_out() {
        let temp = tempfile::tempdir().unwrap();
        let opts = DumpOptions {
            format: DumpFormat::MysqlSql,
            mysql_routines: false,
            mysql_events: false,
            mysql_triggers: false,
            mysql_single_transaction: false,
            ..Default::default()
        };
        let cmd = dump_command(
            "mysql",
            &mysql(),
            &opts,
            Path::new("/tmp/app.sql"),
            temp.path(),
        )
        .unwrap();
        for flag in [
            "--routines",
            "--events",
            "--triggers",
            "--single-transaction",
        ] {
            assert!(
                !cmd.args.iter().any(|a| a == flag),
                "{flag} should be absent"
            );
        }
    }

    #[test]
    fn sqlite_dump_writes_stdout_and_binary_backs_up_the_file() {
        let opts = DumpOptions::default();
        let cmd = dump_command(
            "sqlite",
            &sqlite(),
            &opts,
            Path::new("/tmp/app.sql"),
            Path::new("/tmp"),
        )
        .unwrap();
        assert_eq!(cmd.program, PathBuf::from("sqlite3"));
        assert_eq!(cmd.args[0], "/data/app.db");
        assert_eq!(cmd.args[1], ".dump");
        assert_eq!(cmd.stdout_file.as_deref(), Some(Path::new("/tmp/app.sql")));

        let opts = DumpOptions {
            format: DumpFormat::SqliteBinary,
            ..Default::default()
        };
        let cmd = dump_command(
            "sqlite",
            &sqlite(),
            &opts,
            Path::new("/tmp/app.db"),
            Path::new("/tmp"),
        )
        .unwrap();
        assert_eq!(cmd.args[1], ".backup '/tmp/app.db'");
        assert!(cmd.stdout_file.is_none());
    }

    #[test]
    fn sqlite_restore_reads_sql_or_backs_up_the_archive() {
        let cmd = restore_command(
            "sqlite",
            &sqlite(),
            &RestoreOptions::default(),
            Path::new("/tmp/app.sql"),
            Path::new("/tmp"),
        )
        .unwrap();
        assert_eq!(cmd.args[0], "/data/app.db");
        assert_eq!(cmd.args[1], ".bail on");
        assert_eq!(cmd.args[2], ".read '/tmp/app.sql'");

        let cmd = restore_command(
            "sqlite",
            &sqlite(),
            &RestoreOptions::default(),
            Path::new("/tmp/backup.db"),
            Path::new("/tmp"),
        )
        .unwrap();
        assert_eq!(cmd.args[0], "/tmp/backup.db");
        assert_eq!(cmd.args[1], ".backup '/data/app.db'");
    }

    #[test]
    fn split_args_honours_quotes_and_escapes() {
        assert_eq!(
            split_args(r#"-t "public"."my table" --exclude='a b' c\ d"#),
            vec!["-t", "public.my table", "--exclude=a b", "c d"]
        );
    }

    #[test]
    fn display_quotes_paths_with_spaces() {
        let cmd = ToolCommand {
            program: PathBuf::from("pg_dump"),
            args: vec!["--file".into(), "/tmp/a b.sql".into()],
            env: Vec::new(),
            files: Vec::new(),
            stdin_file: None,
            stdout_file: None,
        };
        assert_eq!(cmd.display(), "pg_dump --file '/tmp/a b.sql'");
    }

    #[test]
    fn tool_connection_debug_hides_the_password() {
        let conn = pg();
        let text = format!("{conn:?}");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("***"));
    }

    #[test]
    fn sqlite_main_is_a_database_node() {
        let main = Node {
            path: crate::node::NodePath::root().child("main"),
            name: "main".into(),
            kind: NodeKind::Schema,
            has_children: true,
            detail: None,
        };
        assert!(is_database_node("sqlite", &main));
        assert!(!is_database_node("postgres", &main));
    }

    #[test]
    fn unsupported_drivers_do_not_get_a_command() {
        assert!(
            dump_command(
                "redis",
                &ToolConnection::default(),
                &DumpOptions::default(),
                Path::new("/tmp/x"),
                Path::new("/tmp")
            )
            .is_err()
        );
    }
}
