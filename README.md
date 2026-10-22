<p align="center"><img src="docs/media/icon.png" width="128" alt="sqlr icon"></p>

# sqlr

A lightweight, fast, universal database client written in native Rust with egui. No webview. The design is in [the spec](docs/spec/sqlr-design.md).

## Status

sqlr connects to PostgreSQL, MySQL/MariaDB and SQLite, edits table data, exports and imports it, edits schemas through forms with a DDL preview, dumps and restores databases, and administers users, sessions and servers. It also browses and edits MongoDB and Elasticsearch documents and Redis keys. Drivers that do not ship with the app run as separate processes and are installed from the Driver Manager: SAP HANA and databases with a JDBC driver through a Java bridge.

### Supported databases

| Database | Driver | Notes |
|---|---|---|
| PostgreSQL (and compatible servers) | `tokio-postgres` | TLS (`disable` … `verify-full`, rustls), SSH tunnel |
| MySQL 8 / MariaDB 11 | `mysql_async` | TLS (rustls), SSH tunnel |
| SQLite 3 | `rusqlite` (bundled) | files, read-only open |
| MongoDB | `mongodb` (rustls) | the [Mongo shell subset](docs/mongo-shell-subset.md), TLS, SSH tunnel |
| Elasticsearch 7.17 / 8.x | `reqwest` (rustls) | Kibana-console requests, ES\|QL and SQL, basic or API-key auth, SSH tunnel |
| Redis (standalone) | `redis` (rustls) | key browser, value views, console, TLS, SSH tunnel; clusters are refused |
| SAP HANA | `hdbconnect_async` sidecar (installable) | TLS with a custom CA, cancel, redirect pin; see [docs/drivers/hana.md](docs/drivers/hana.md) |
| SQL Server, Oracle, Db2, ClickHouse, Snowflake, DuckDB, H2, any JDBC | JDBC bridge sidecar (installable, Java 17+) | see [docs/drivers/jdbc.md](docs/drivers/jdbc.md) |

A connection can also be filled in by pasting a URL (`postgresql://user@host:5432/db`, `mysql://…`, `sqlite:/path`, `mongodb://…`, `elasticsearch://…`, `redis://…`); a password inside a URL is refused.

### Drivers

**Settings → Drivers…** (or the command palette's *Driver Manager…*) lists installed, available and built-in drivers, with versions, **Update**, **Activate** (rollback) and **Remove**. The registry is only contacted when you open the manager or accept an install; there are no background update checks. Registry downloads must match their sha256 and verify against the project's minisign key, and a redirect to another host is refused. **Install from file…** installs a driver archive you picked after showing its checksum and a warning, and only after **Install anyway**. A saved connection whose driver is not installed shows *Driver … is not installed* with an **Install** button, and the new-connection catalog marks installable databases. A driver runs as its own process, started on the first connection and stopped five minutes after the last one closes (Settings); if it crashes only its connections fail.

Write your own in Rust with `sqlr-driver-sdk` (`run(MyDriver)`): generate a skeleton from `templates/sqlr-driver-template`, check it with the conformance suite in `sqlr-driver-tests`, and see [docs/writing-a-driver.md](docs/writing-a-driver.md) and [docs/driver-protocol.md](docs/driver-protocol.md). `tools/sign-index` signs a registry index; `scripts/package-driver.sh` packs the HANA and JDBC drivers. No registry is published yet and the project key in the app is a placeholder, so installs go through **Install from file…** for now.

### Connections and folders

Saved connections can be grouped in nested folders in the explorer, all closed when the app opens: the header's folder button creates one inside the selected folder (or at the top level), a folder's context menu creates one inside it, rename with F2, and drag connections and folders to reorder them or move them inside a folder (drop a folder on the middle of another). Deleting a folder keeps everything: its connections and subfolders move to the folder's parent. The arrangement is saved in `connections.toml`.

### Profiles and commit modes

Each connection has a profile (Development, Test, Production or your own) that sets its color, its commit mode, read-only, and which statements ask for confirmation. Commit modes:

- **Auto**: every statement commits on its own.
- **Manual**: a transaction is open until you press Commit or Roll back; savepoints are available.
- **Smart**: auto-commit until the first statement that writes, then a transaction until you end it.

Each editor tab runs on its own session (a setting turns this off), and read-only can be switched per session; the server enforces it where it can.

### Settings

⌘, opens searchable Preferences, with General, Appearance, Editor, Result grid, Data formats, Keymap, Connections and Drivers sections. Settings resolve from connection overrides to profile overrides, global values and defaults. Choose the scope at the top; an override dot identifies locally set values, and Reset clears one override. Read-only and confirmation flags can only be tightened below the global level. Changes preview live, Cancel restores the last applied values, Apply saves in the background, and Save closes after persistence succeeds.

Data formats configure timestamps, numbers, booleans, NULL, binary and JSON presentation. Text exports can optionally use the display formats; SQL/JSON exports and clipboard copies keep their raw representations. Existing flat settings migrate automatically, with `settings.toml.bak-m9` preserved before the first write. Corrupt settings are retained as `.corrupt` and reported in a toast.

Keymap offers **sqlr** and **VS Code** presets, action/shortcut search, recording, removal and reset. Conflicts must be resolved before saving. Menus, command palette and shortcut help show the current bindings. Basic character input, deletion, arrow navigation and contextual dialog dismissal remain fixed.

SQL formatting (⌥⌘L in the sqlr preset) applies to the selection or whole buffer as one undo group. Preferences control keyword case, indentation, line length, comma position and clause breaks, plus autocomplete triggers, delay, Tab acceptance and inserted keyword case. String bodies, comments and quoted identifiers are preserved.

The **Operations** panel lists client work: connect attempts, catalog reads, query/fetch work, scripts and background jobs. Open it from the activity count, File → Operations, the command palette or ⇧⌘O. Cancel interrupts one operation; Cancel all asks first. Finished rows remain for 30 seconds by default (General preferences), independently of the owning tab's result. A hung connect can be cancelled and retried.

Connection forms expose typed **Properties**: PostgreSQL startup settings (`application_name`, `statement_timeout`, `search_path`, `options`); MySQL session variables (`sql_mode`, `time_zone`, `wait_timeout`); SQLite pragmas (`foreign_keys`, `journal_mode`, `busy_timeout`, `cache_size`). PostgreSQL/MySQL additionally accept validated free-form keys. Driver defaults and layered overrides live under `drivers.<driver>.*`; extra defaults are JSON string maps in `drivers.<driver>.properties`. Saved connection properties override driver/profile defaults. Secrets stay in the keychain.

### Editing data

A result read from one table that has a primary key (or a unique key over `NOT NULL` columns) can be edited in the grid: double-click or ↵ edits a cell (long, JSON and binary values open in the value panel, where JSON is highlighted and checked as you type like the query editor and **Beautify** reindents it), and the context menu adds, duplicates, deletes and reverts rows and sets NULL. Changes are tinted until applied, and the result's status bar at the bottom shows the pending count with **Preview** (the generated SQL), **Apply** (in a transaction, inside the open one in Manual and Smart mode) and **Discard**, adds a row of defaults, and exports the result. If a row was changed or deleted by someone else meanwhile, nothing is applied and the row is named. When a result cannot be edited, the grid says why.

The grid also has row numbers, rectangular selection (click, ⇧-click, drag, ⇧-arrows, ⌘A), copy as TSV, CSV, JSON, SQL `INSERT` or Markdown, sorting by header click (locally for complete results, on the server otherwise), and a WHERE field that re-runs the query filtered on the server. Each run has a Messages tab with every statement's outcome, timing and server notices.

### Export and import

Results and tables export to CSV, TSV, JSON, JSON Lines, SQL `INSERT` and Markdown through a save dialog: either the loaded rows or the whole result (re-run and streamed; cancellable, and no file is left behind on cancel). CSV, TSV, JSON and JSON Lines import into an existing table with a preview, a column mapping (matched by name), batches in one transaction or one per batch, an option to skip bad rows and list them by line, and an option to delete every row of the table first. Under profiles that confirm destructive statements, deleting first asks; under profiles that confirm writes, the import itself asks.

### Dump and restore

A database's context menu (**Dump database…**, **Restore database…**) runs the database's native tools: `pg_dump`/`pg_restore`/`psql` for PostgreSQL, `mysqldump`/`mysql` for MySQL and MariaDB, and `sqlite3` for SQLite. The dialog picks the format (plain SQL, custom, tar or directory for PostgreSQL; SQL for MySQL; SQL or a file copy for SQLite) and the usual options: schema-only or data-only, DROP/CREATE statements, owner and privileges, encoding, compression and parallel jobs. A dump writes a hidden partial file and moves it into place only when the tool succeeds; a cancelled or failed dump leaves nothing behind. The engine shows the exact command line, streams the tool's log and progress, and kills the process on cancel. A custom executable path and extra arguments are available under **Advanced**.

SQLite file copies use SQLite's online backup API, so the database can stay in use and no `sqlite3` is needed for them; a cancel kills a native tool together with the processes it started.

Passwords never appear on a command line: PostgreSQL tools get `PGPASSWORD` in their environment and MySQL tools a temporary owner-only defaults file removed when the tool ends. Tools are searched on `PATH`, then in Homebrew, Postgres.app, `/usr/local/bin` and the usual PostgreSQL/MySQL directories; a missing tool is reported with how to install it. SSH connections run the tools through the connection's tunnel. Document and key-value databases do not offer this menu.

### Data transfer and tasks

**Transfer data…** (a table's context menu, or ⌘K) opens a wizard that copies rows between any two connections, or between a connection and a file. The source is a table, a query that only reads, or a file (CSV, TSV, JSON, JSON Lines, Excel `.xlsx` with a sheet picker, Parquet); the target is an existing table, a new table or a file in any of those formats plus SQL `INSERT`. Text files can be UTF-8, UTF-16 (by its byte-order mark) or any label such as `latin1`. The mapping step pairs columns by name and lets you rename, skip, force a type, or trim, recase, null-if-empty or fill with a constant. A new table is created from the source's columns with their types mapped to the target dialect (PostgreSQL, MySQL, SQLite); warnings say what was lost, and the `CREATE TABLE` is editable before it runs. Options: batch size, commit once or per batch (a cancel keeps and reports the committed batches), delete the target's rows first, update-by-key upserts, switching foreign keys off during the load (restored afterwards), and skipping bad rows with a rejected-rows file. Data moves in bounded batches, so a very large file does not fill memory.

Every wizard's last step, and the dump and restore dialogs, can **Save as task**. The **Tasks** pane lists tasks by kind with Run, Edit, Duplicate and Delete, the history of each task's runs (outcome, time, summary, full log) and a Cancel for a running one. A task stores connection ids and options, never secrets; running it connects what it needs and fails with a precise message when a connection, input file or output directory is missing.

Notes: wide or unspecified decimals going into SQLite stay text (SQLite would turn them into 64-bit floats); timestamps with a UTC offset are folded into UTC for `datetime` columns; a source and a target on the same connection are not supported (use `INSERT … SELECT`); an Excel sheet is read whole (Excel caps a sheet at about a million rows).

### Schema tools

**Compare with…** (a schema or database node, or ⌘K) compares two schemas of one kind of database, on the same or two connections. A tree lists the objects that were added, removed or changed (filter by status, name or views; uncheck an object to leave it out), with a side-by-side DDL diff; **Open migration script** writes the script for the target into an editor tab — it is only text, nothing is applied. The script orders itself so it runs: tables before their foreign keys (circular and self references included), views after what they read, drops last and in reverse; destructive steps are listed at its head. Spelling-only differences (`int4` against `integer`, whitespace in a view) are not differences. Schemas are read in a fixed number of catalog queries, so thousands of tables are fine.

**ER diagram** (a schema node) draws its tables and foreign keys: automatic layered layout, drag boxes (the positions are saved per schema), pan and zoom, keys only or all columns, export to SVG. **Explain** and **Explain Analyze** (Query menu, ⌘K) show the plan of the statement under the caret as a tree with cost, estimated against actual rows (a badge when they are 10× apart), time per node, the three heaviest nodes highlighted and the raw plan one click away. Analyze runs the statement: a data change asks first and is rolled back (PostgreSQL); MySQL/MariaDB analyze only queries; SQLite offers the query plan.

**Generate data…** (a table) fills it with generated rows: a generator per column chosen from its name and type (names, emails derived from them, dates, numbers, UUIDs, patterns, lists, lorem text; foreign keys pick keys that exist; unique columns never repeat and fail with a clear message when the domain is too small), NULL ratios, a seed (the same seed gives the same rows), a preview of 20 rows, and a write through the transfer pipeline in bounded batches. It can be saved as a task.

### Object editors

The explorer's **Open editor** (or ⌘P "Edit …") opens tables, views, materialized views, indexes, sequences, functions, procedures, triggers, schemas and databases in a tab with sub-tabs: Columns, Indexes, Constraints, Foreign keys, Triggers, Partitions (PostgreSQL), Definition, Properties (with the table's statistics), Permissions (grants per role), DDL and Data. Edits stay pending until **Apply** (⌘S); **Preview DDL** shows the statements first. PostgreSQL and SQLite apply in one transaction; MySQL applies statement by statement and reports which ones ran. SQLite changes its `ALTER TABLE` cannot express rebuild the table, keeping data, indexes, triggers and views.

**New ▸** creates objects of the kinds the driver supports; **Drop…** and **Truncate…** always ask, with the statements shown; F2 renames inline. **Generate SQL ▸** opens SELECT, INSERT, UPDATE, DELETE, CREATE or DROP templates in an editor tab, and **Copy DDL** copies an object's definition.

### Administration

A connected connection's context menu (and ⌘K) opens, where the driver supports them:

- **Users and roles**: the roles with their attributes, memberships and password (masked in previews, never stored), and a privilege grid per role (objects × privileges, each cell not granted, granted, or granted with grant option). Changes are previewed as `CREATE`/`ALTER ROLE` (MySQL: `CREATE`/`ALTER USER` for `user@host` accounts) and `GRANT`/`REVOKE`, and applied together. Revoking asks first under profiles that confirm destructive statements; dropping a role always asks, twice for the user you are connected as.
- **Sessions and locks**: the server's sessions, refreshed every 2 s while the tab is shown (1–60 s, or paused), filterable by user, database and idle state. sqlr's own sessions are marked "this connection". **Cancel query** and **Terminate** always ask, twice for one of sqlr's own sessions. The selected session's query shows in the value panel. Locks list what each session waits for, with a blocking tree.
- **Server properties**: settings by category with search (SQLite: its PRAGMAs and an integrity check), database sizes, and table statistics per schema (rows, total, data and index sizes, scans, last vacuum and analyze), sortable.

SQLite has no users or sessions, so only Server properties is offered for it. On MySQL and MariaDB the privilege grid covers databases, tables and routines but not global privileges (`ON *.*`): grant and revoke those with `GRANT … ON *.*` in a query tab.

### NoSQL

- **MongoDB and Elasticsearch** open in document tabs. The editor speaks the connection's language: the [Mongo shell subset](docs/mongo-shell-subset.md) (parsed by sqlr, no JavaScript engine) or Kibana-console requests (`GET logs/_search` plus a JSON body, `ESQL …`, `SQL …`). ⌘↵ runs the statement at the caret, ⇧⌘↵ the script. Opening a collection or index from the explorer runs its first page.
- Results show as a **Table** (the union of the first page's top-level keys, nested values as compact JSON), a **Tree** (expanded one level at a time) or **JSON**; the choice is the tab's. A status bar counts documents and offers Fetch more and Fetch all, which stops at the per-result memory budget (256 MB). The value panel shows the selected field.
- **Editing**: the documents of a plain `find()` (no projection) or of a search can be edited in the JSON view (relaxed Extended JSON, `ObjectId(…)`, `ISODate(…)` and friends accepted), checked before Save, and saved by `_id` (on Elasticsearch, by the hit's concrete `_index`). Insert, Duplicate and Delete (with a confirmation) are in the menus. A document holding a type sqlr cannot write back (JavaScript code, symbols, DBPointers) stays read-only. The documents of a MongoDB view are read-only. Read-only profiles refuse every write; the connection profile confirms writes and destructive statements as for SQL.
- **Indexes**: the explorer creates MongoDB indexes (keys and directions, unique, TTL, partial filter) and Elasticsearch indices (settings and mappings JSON), and drops them.
- **Redis** opens a key browser (`SCAN` only, never `KEYS`) with typed value views, TTL editing and a console. The toolbar's Read-only switch applies to the connection's keys and console; a key-value tab has no transactions. Known limitation: **Cancel** in one console interrupts the running commands of every console of that connection, and consoles on the same database share one server connection, so a blocking command (`BLPOP`, `WAIT`, …) in one tab holds the other tabs' commands until it ends or is cancelled.
- A SQL tab is only pointed at connections that run SQL; history entries and snippets of a MongoDB or Elasticsearch connection reopen in a document tab, and those of a Redis connection in its console.
- **Transfer**: collections, indices and Redis keys export (documents whole in JSON and JSON Lines, flattened in the other formats; Redis keys as name, type, TTL and value as JSON); CSV, TSV, JSON and JSON Lines import into a collection as flat documents. The engine also moves a collection into a table (`Engine::transfer`).

## Editor

The editor is built for large scripts: it only lays out the visible lines, and a 10 MB file scrolls and takes typing in well under a frame. It numbers the lines, tints the statement holding the caret, and (with soft wrap on, from Settings) breaks long lines at the editor's width instead of scrolling sideways: Up and Down then move by visual row. Lines longer than 2 KB are never wrapped; they scroll horizontally and their window assumes a monospace advance. On macOS the shortcuts use ⌘; elsewhere use Ctrl.

| Keys | Action |
|---|---|
| ⌘↵ | Run the selection, or the statement at the caret |
| ⇧⌘↵ | Run the script (the selection, or the whole text) |
| ⌘P | Open a table, view or other object from any connection |
| ⌘K | Command palette: app commands, snippets and history |
| ⌘T / ⌘N | New query tab |
| ⌘W | Close the tab |
| ⌘, | Preferences |
| ⌥⌘L | Format selection or SQL buffer |
| ⇧⌘O | Open Operations |
| ⌘D | Duplicate line or selection |
| Ctrl+Tab / Ctrl+⇧Tab | Next / previous tab |
| Ctrl+Space | Open or close completion (it also opens while you type, and after `.`) |
| Tab / ⇧Tab | Indent / outdent |
| Esc | Close completion, then the find bar, then extra carets, then the selection; with nothing left to close, stop the running query |
| ⌘Z / ⇧⌘Z (⌘Y) | Undo / redo |
| ⌘A | Select all |
| ⌘/ | Toggle line comment |
| ⌘F / ⌥⌘F | Find / find and replace |
| ⌘G / ⇧⌘G | Next / previous match |
| ⌥← ⌥→ | Move by word (Ctrl+arrows off macOS) |
| ⌘← ⌘→ | Line start / end (Home / End off macOS) |
| ⌘↑ ⌘↓ | Start / end of the text (Ctrl+Home / Ctrl+End off macOS) |
| ⌥⌘↑ ⌥⌘↓ | Add a caret above / below |
| ⌥-click | Add a caret |
| PageUp / PageDown | Move by page |

History, snippets and other local data live in `sqlr.db` at `<data dir>/sqlr.db` (see below). A history entry's context menu opens it in a new tab (on its connection and the server database it ran on; a connection that is no longer saved opens the tab without one, with a toast), inserts it into the editor, copies its SQL, or clears the history of the selected connection (or of every connection).

To re-run the editor performance check (10 MB file, p99 per scenario against the 8 ms bar):

```sh
cargo run --release -p sqlr-ui --example editor_measure
```

It prints a table and writes `target/editor-measure.json`.

## Build and run

```sh
cargo build --release -p sqlr-app
cargo run --release -p sqlr-app
```

`sqlr --help` lists the flags: `--config-dir <path>` reads connections, profiles and settings from `<path>` and `--data-dir <path>` keeps history, snippets, logs and SSH known hosts there, so a second profile is just another pair of directories. The welcome screen's **Open demo database** builds a small SQLite database with sample tables (deterministically, once) and opens it, for trying the app without a server.

## Tests

```sh
cargo test --workspace
```

The PostgreSQL, MySQL, MariaDB, MongoDB, Elasticsearch and Redis integration tests start containers with testcontainers. On Docker Desktop for macOS, set the socket first:

```sh
DOCKER_HOST=unix://$HOME/.docker/run/docker.sock cargo test --workspace
```

## Where things live

Paths come from the platform's standard directories for the `dev.sqlr.sqlr` application. On macOS both directories below are `~/Library/Application Support/dev.sqlr.sqlr/`; on Linux they are `~/.config/sqlr/` and `~/.local/share/sqlr/`.

| What | Where |
|---|---|
| Connections (no secrets) | `connections.toml` in the config directory |
| Global settings | `settings.toml` in the config directory |
| Profile / connection setting overrides | `overrides.toml` in the config directory |
| Shortcut preset and overrides | `keymap.toml` in the config directory |
| Connection profiles | `profiles.toml` in the config directory |
| Installed drivers | `drivers/<id>/<version>/` in the data directory |
| Secrets (passwords) | the OS keychain, service name `sqlr`, never in a file |
| History, snippets and other local data | `sqlr.db` in the data directory (`<data dir>/sqlr.db`) |
| Logs | `logs/sqlr.log.<date>` in the data directory, also printed to stderr |

Set `RUST_LOG` to change the log level (default `info`).

## License

MIT, see [LICENSE.md](LICENSE.md).

### Continuous integration and performance

CI runs formatting, clippy, dependency constraints and the workspace tests on Linux and macOS. Linux integration tests own their Docker containers; macOS CI sets `SQLR_SKIP_CONTAINER_TESTS=1` for those entry points. Native startup, release size, million-row grid scrolling and the existing editor budgets are checked on both platforms. RSS is measured but its 150 MB gate is deferred pending optimization; `SQLR_ENFORCE_RSS_BUDGET=1` restores strict enforcement. See [performance budgets](docs/performance-budgets.md) for commands, measurements and limitations.
