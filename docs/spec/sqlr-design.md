# sqlr — Product specification

- **Scope:** Product behavior, architecture, supported databases and security.

## 1. Purpose

sqlr is an open-source, universal database client for relational and NoSQL databases. Its main differentiator is that it is **lightweight and fast** while providing a complete set of database development tools.

### 1.1 Goals

- One native app for relational, document and key-value databases.
- Measurable lightness (see §1.3). These are hard requirements, not aspirations.
- A modular driver system. Adding a database means writing a driver without changing the app. Drivers that do not ship with the app install from a local driver file.
- Open source and community-extensible.

### 1.2 Non-goals

The product excludes:

- Redis Cluster.
- Process sandboxing for third-party drivers.
- Telemetry of any kind.
- Scheduled (cron-like) execution of saved tasks; saved tasks run on demand.
- Installers, code signing and notarization are outside the current scope.

### 1.3 Performance targets

These are enforced in CI (§11.5).

| Metric | Target |
|---|---|
| Cold start to interactive window | < 1 s |
| RAM at idle, 1–2 open connections | < 150 MB |
| Result grid scrolling | 60 fps with 1M+ rows loaded |
| Autocomplete popup / open table | < 50 ms perceived |
| Installed binary size (bundled drivers included) | < 50 MB, no external runtime |

These targets rule out Electron, any webview, and the JVM. They are why the UI is native Rust.

## 2. v1 feature set

1. **Connection management:** saved connections organized in folders, moved between folders and reordered by drag and drop, SSH tunnels, TLS, secrets in an encrypted vault protected by the OS keychain, a "production" flag and a read-only mode.
2. **Schema explorer:** a lazy tree of databases, schemas, tables, views, indexes, functions and so on (or the NoSQL equivalents).
3. **Query editor:** syntax highlighting, schema-aware autocomplete, multiple tabs, run the current statement, the selection or the whole script.
4. **Result grid:** virtualized, with sorting and filtering.
5. **Inline data editing:** a pending changeset, DML preview, then apply inside a transaction with commit or rollback.
6. **Native NoSQL viewers:** table, tree and JSON views for documents (MongoDB, Elasticsearch), and a typed key browser for Redis.
7. **Export/import:** CSV, JSON, SQL.
8. **Query history and saved snippets.**
9. **Command palette:** ⌘P "open object" and ⌘⇧P "run command".

### Additional product features

10. **Object editors:** forms to create and alter tables, columns, indexes, constraints, views, materialized views, functions/procedures, sequences, triggers, schemas and databases, always with a DDL preview before applying (§8.4).
11. **DDL generation** for any object, and "generate SQL" helpers (SELECT/INSERT/UPDATE/DELETE templates, CREATE from an existing object).
12. **Administration:** users, roles and grants; active sessions and locks with cancel/terminate; server and database properties and statistics (§8.4).
13. **Data tools:** guided dump and restore, data transfer between any connections and files, advanced import/export, saved re-runnable tasks (§8.5).
14. **Schema tools:** schema/database compare with a generated migration script, ER diagrams, visual explain plans, a test-data generator (§8.6).
15. **Generic JDBC bridge** sidecar, so any database with a JDBC driver can be connected (§6).
16. **Configurability:** the preferences window covers the editor, grid, data formats, keymap, themes and per-connection driver properties (§8.7). The client-side operations monitor supports per-operation cancellation, including a stalled connection attempt (§8.3).

17. **Per-editor sessions:** each editor tab runs on its own connection (one more connection for metadata), so a long statement in one tab never blocks another tab or the explorer. A setting turns it off, using one shared session per connection.
18. **Transactions:** savepoints (create, roll back to) in Manual and Smart mode, and a per-session **read-only** switch next to the commit mode (§7.9).
19. **Connection setup:** paste a connection URL (`postgresql://user@host:5432/db`, `mysql://…`, `sqlite:/path`) to fill the form (a password embedded in a URL is refused, URLs are redacted in logs); "Save password" toggle; a Test that reports server version and round-trip time.
20. **Known databases catalog:** the new-connection dialog and the welcome screen list every database sqlr knows about, each marked *bundled*, *compatible* (served by a bundled driver: CockroachDB, YugabyteDB, Redshift and Greenplum through PostgreSQL; MariaDB, TiDB and SingleStore through MySQL) or *planned* when support is not available yet. Nothing is listed as working that is not.
21. **Welcome screen and demo database:** with no connection open, a welcome view offers driver cards, "Open demo database" (a deterministic SQLite sample generated on first use in the data directory) and the recent connections.
22. **Chrome:** a main toolbar with an app mark, a connection chip (switch, connect, edit, reconnect, disconnect), a combined Run/Stop button and Run script; a breadcrumb under the tab strip (connection › database › schema › tab); toasts for short confirmations and background errors; a status bar with connection state, commit mode and transaction state, Ln/Col, rows · time and encoding.
23. **Explorer polish:** a header with the connection count and filter/refresh/add actions; object counts on folder rows; driver or file detail on connection rows; an inline connection error with Retry; context menus (new console, select 100 rows, open DDL, copy name, refresh, edit, disconnect, remove).
24. **DDL viewer:** "Open DDL" shows an object's DDL in a read-only editor tab (from the server where it can produce it: `sqlite_schema.sql`, `SHOW CREATE`, `pg_get_*def`). The same DDL action is available for all supported object types.
25. **Results, extended:** a row-number column, Messages and Plan panes per run (statements, timings, affected rows, notices, truncation; the visual plan), a client-side row filter field, per-statement timings, and typed rendering for every value class (null, bool, integers, exact decimals, floats, text, bytes, JSON, XML, date/time/timestamp(tz)/interval, UUID, arrays, geometry) with NULL in italic grey and numbers right-aligned.
26. **Editor, extended:** a tint on the statement under the caret, optional soft wrap, optional line numbers, and a "Query history" view that can insert into the editor, copy, clear, and reopen an entry with its original connection and database.
27. **Preferences, extended:** UI scale, editor font size, line numbers, soft wrap, result row cap, page size, theme; "Open settings folder"; command-line `--version` and `--help`.

### Target databases

| Database | Family | v1 delivery |
|---|---|---|
| PostgreSQL | Relational | Bundled |
| MySQL / MariaDB | Relational | Bundled |
| SQLite | Relational (file) | Bundled |
| MongoDB | Document | Bundled |
| Elasticsearch | Document (+ tabular query) | Bundled |
| Redis | Key-value | Bundled |
| SQL Server | Relational | Bundled (`tiberius`) |
| Oracle | Relational | Bundled (`oracle-rs`, no OCI) |
| SAP HANA | Relational | Install from a file (sidecar) |
| Any database with a JDBC driver (Db2, ClickHouse, Snowflake, DuckDB, H2, …) | Relational | Install from a file (JDBC bridge sidecar, needs Java 17+) |

## 3. Technology choices

- **Language:** Rust (stable).
- **UI:** `egui` via `eframe` with the wgpu backend, and `egui_dock` for dockable panels. No webview anywhere.
- **Async runtime:** `tokio`.
- **TLS:** `rustls` only, no OpenSSL. Known limitation: TLS 1.0 and 1.1 servers are not supported, and the docs say so.
- **SSH:** `russh`.
- **Local storage:** `rusqlite` with SQLite compiled in.
- **Secrets:** an AES-256-GCM vault using `ring`; `keyring` protects its encryption key.
- **Syntax:** a resumable per-line lexer (per dialect) for highlighting; `tree-sitter` for statement-scoped structure (autocomplete context, statement under the caret), parsed over the statement(s) around the caret and off the UI thread when the region exceeds 64 KB.
- **Text buffer:** `ropey`.
- **License:** MIT (`LICENSE.md`).

### 3.1 Known egui risks (accepted)

1. **The code editor.** `egui::TextEdit` re-lays out the entire text and has no multi-cursor, completion popup or serious undo, so it cannot handle large files. A dedicated `QueryEditor` widget handles these needs (§8.1).
2. **Non-native look and fonts.** The app looks the same on every OS, as Zed or Rerun do. This is mitigated with carefully designed light and dark themes. egui's bundled fonts have no CJK or emoji glyphs, so the app loads system fallback fonts at startup.
3. **Accessibility and IME.** Supported through AccessKit and egui's IME handling, but they must be verified on every supported platform.

## 4. Architecture

### 4.1 Cargo workspace

| Crate | Responsibility |
|---|---|
| `sqlr-core` | Domain types (`Value`, `Node`, `NodePath`, `ResultChunk`, `ConnectionConfig`, `DriverError`), the driver traits and the chunk format. No UI and no I/O. |
| `sqlr-driver-sdk` | A public crate that re-exports `sqlr-core`'s driver API plus `serve(impl Driver)`, which turns a driver into a sidecar executable. |
| `sqlr-driver-postgres` | Bundled driver (`tokio-postgres`). |
| `sqlr-driver-mysql` | Bundled driver (`mysql_async`). |
| `sqlr-driver-sqlite` | Bundled driver (`rusqlite`, SQLite compiled in). |
| `sqlr-driver-mongo` | Bundled driver (official `mongodb` crate). |
| `sqlr-driver-elastic` | Bundled driver (REST via `reqwest`). |
| `sqlr-driver-redis` | Bundled driver (`redis-rs`). |
| `sqlr-sidecar` | The sidecar protocol: the codec and the core-side `SidecarDriver` client. |
| `sqlr-engine` | The tokio runtime, sessions and pools, query execution and cancellation, `ResultStore`, the catalog cache and installed-driver management. |
| `sqlr-store` | Connections (TOML), secrets (encrypted vault with a keychain-protected key), and history, snippets, catalog cache and layout (SQLite). |
| `sqlr-ui` | Reusable egui widgets: `QueryEditor`, `ResultGrid`, `SchemaTree`, `DocTree`, `JsonView`, `KeyBrowser`, `KeyValueView`, `ValuePanel`, `Palette`. |
| `sqlr-app` | The binary: eframe shell, layout, wiring and settings. |

Bundled drivers are Cargo features of `sqlr-app`, so a build without a given driver is just a feature flag.

### 4.2 Dependency rules

- `sqlr-core` depends on nothing internal.
- Drivers depend only on `sqlr-core` (or on `sqlr-driver-sdk`).
- `sqlr-engine` depends on `sqlr-core`, `sqlr-sidecar` and `sqlr-store`. It sees the drivers only as trait objects.
- `sqlr-ui` depends only on `sqlr-core`. Widgets take data in and return actions. They never call the engine.
- `sqlr-app` depends on everything and is the only place where things are wired together.

### 4.3 Threading model

- **UI thread:** runs egui. It never performs I/O and never blocks. It sends `Command`s to the engine over a channel and reads shared state (`ResultStore`, catalog cache) through cheap snapshots.
- **Engine:** a multi-threaded tokio runtime. It runs commands, streams results into the `ResultStore` and calls `ctx.request_repaint()`, throttled to about 60 Hz.
- **Cancellation:** every operation carries a `CancelToken`; a connect attempt carries one too, so it can be dropped before its timeout. "Stop" triggers a real server-side cancel where the capability exists (`pg_cancel_backend`, `KILL QUERY`, Mongo `killCursors`, and so on), then drops the stream. Every in-flight operation is registered with an `OperationId`, so the Operations panel (§8.3) can list it and cancel it individually.

## 5. Driver model

### 5.1 Traits

```rust
trait Driver: Send + Sync {
    fn manifest(&self) -> &DriverManifest;
    async fn connect(&self, cfg: &ConnectionConfig) -> Result<Box<dyn Connection>, DriverError>;
}

trait Connection: Send + Sync {
    async fn ping(&self) -> Result<ServerInfo, DriverError>;
    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError>;
    fn cancel_handle(&self) -> CancelHandle;
    fn family(&self) -> FamilyConn<'_>; // Relational | Document | KeyValue
}
```

`DriverManifest` contains:
- `id`, display name and family;
- the `capabilities` flags;
- the connection-form schema;
- the `Dialect` id;
- the protocol version (for sidecars).

### 5.2 Families

| Family | Operations |
|---|---|
| **Relational** | `execute(sql, params) → Stream<ResultChunk>`; `describe(table)` (columns, indexes, foreign keys, primary or unique key); `catalog(schema)` for autocomplete; `begin` / `commit` / `rollback`; `dialect()` |
| **Document** | `find(coll, filter, sort, projection, limit) → Stream<DocChunk>`; `run(native_query)`; `insert` / `update` / `delete` documents; `indexes(coll)` |
| **KeyValue** | `scan(pattern, cursor, count) → (Vec<KeyInfo{name, type, ttl}>, cursor)`; `get(key, range)` returning a typed value with paging for large collections; `set`, `del` and `expire`; `command(raw)` |

### 5.3 Capabilities

These are flags such as `transactions`, `server_cancel`, `editable_results`, `explain`, `multi_resultset`, `tabular_query`, `schemas` and `ssh`. The UI shows an action only when the driver declares the matching capability.

### 5.4 Dialect

Each relational driver provides a `Dialect` with:
- identifier quoting;
- statement splitting;
- keywords and built-in functions, used for highlighting and completion;
- the tree-sitter grammar id;
- DML generation for the inline-edit changeset;
- query wrapping for server-side sort and filter;
- classification of destructive statements, used by the production safeguards.

### 5.5 Connection form

The manifest declares its fields: type, label, default, required, secret flag, and grouping into tabs such as Main, SSH, TLS and Advanced. The app generates the form from them, so a community driver gets a working form without writing UI code. Fields marked as secret are stored in the encrypted credential vault, never in the TOML file.

### 5.6 Per-database notes

- **PostgreSQL:** `tokio-postgres` with the binary protocol. It uses portals with row limits for incremental fetching and native cancel. The connection form offers **Show all databases**: the explorer then lists every connectable database, and each database browsed or queried on demand gets its own connection (same server and credentials).
- **MySQL / MariaDB:** `mysql_async` with row streaming and `KILL QUERY` for cancellation.
- **SQLite:** `rusqlite` with the `bundled` feature (the same SQLite the app already compiles in). The connection form has a file path (with a file picker) and a "read-only" option instead of host and port; no secrets. Blocking calls run on a dedicated thread per connection; rows are stepped incrementally from a prepared statement. Cancellation uses `InterruptHandle::interrupt()`. Schemas are the attached databases (`main`, `temp`, plus any `ATTACH`ed file), browsed through `sqlite_schema` and `pragma_table_info`/`pragma_index_list`. Values keep SQLite's dynamic typing: each cell is decoded by its storage class.
- **MongoDB:** the official driver. The query language is a **subset of the shell syntax**: `db.<coll>.find|aggregate|countDocuments|insertOne|insertMany|updateOne|updateMany|deleteOne|deleteMany(...)` with relaxed JSON and Extended JSON literals such as `ObjectId(...)` and `ISODate(...)`. It is parsed by our own small parser; there is no JavaScript engine.
- **Elasticsearch:** REST through `reqwest`. It supports Query DSL (`GET index/_search {…}`), ES|QL and SQL. ES|QL and SQL return tabular results, so the driver declares `tabular_query` and their results go to the normal grid. Incremental fetching uses PIT plus `search_after`.
- **Redis:** `redis-rs` against a standalone server, with TLS and ACL. The key browser uses `SCAN` only and never `KEYS`. Keys are grouped by a configurable separator (default `:`). Large values are paged with `HSCAN`, `SSCAN`, `ZRANGE`, `LRANGE` and `XRANGE`.
- **SAP HANA:** a downloadable Rust sidecar using `hdbconnect_async` (the driver is implemented). The driver:
  - converts PEM CAs to DER for `ServerCerts::Directory` (the `Direct`/`Environment` variants mis-handle PEM);
  - installs a rustls crypto provider;
  - cancels with `ALTER SYSTEM CANCEL SESSION '<connection id>'` on a side connection;
  - offers "pin redirect to original host" for DBConnectInfo redirects behind Docker, NAT or SSH;
  - offers "skip certificate verification" for default HXE certificates.

  `SAP/go-hdb` in a Go sidecar remains the fallback if `hdbconnect` maintenance stalls.

## 6. Modular drivers: sidecars, SDK, local driver files

### 6.1 Why sidecars

| Option | Verdict |
|---|---|
| Dynamic libraries (`cdylib`) | Rejected. Rust has no stable ABI, so mixing compiler versions breaks drivers, and a driver crash would take down the app. |
| WASM components | Rejected. Drivers need real TCP and TLS, WASI socket support is still immature, and the runtime adds 15–20 MB. |
| **Out-of-process sidecar** | ✅ Isolated, can be written in any language, and ships as a normal per-platform binary. |

IPC cost is negligible: binary columnar chunks over pipes run at hundreds of MB/s, far faster than the network link to the database. Each active sidecar costs about 5–15 MB of RAM.

### 6.2 Protocol

- **Transport:** the sidecar's stdin/stdout, carrying length-prefixed frames. stderr is captured into the app log.
- **Control messages** are MessagePack. They cover the handshake (protocol version, the manifest), `connect`, `browse`, `execute`, `fetch_more`, `cancel`, `close` and the family operations.
- **Data messages** carry `ResultChunk` / `DocChunk` in the same binary format the in-process drivers use (§7.2), so there is no transcoding.
- **Versioning:** a semver protocol version is negotiated in the handshake. An incompatible driver is reported and not started.
- **Lifecycle:** a sidecar is spawned on the first connection that needs it and stopped after an idle timeout. If it crashes, only its connections fail with a `Protocol` error and a "Restart driver" action; the app is unaffected.
- **Core side:** `SidecarDriver` implements the same `Driver` and `Connection` traits, so the engine and UI cannot tell a sidecar from an in-process driver.

### 6.3 Driver SDK

- `sqlr-driver-sdk` exposes the traits, the core types and `serve(MyDriver)`. `serve` provides the handshake, framing, cancellation and chunk encoding.
- A Rust driver crate compiles in two ways:
  - **in-process**, as a bundled feature of `sqlr-app`;
  - **as a sidecar binary**, through a 3-line `main.rs` that calls `serve`.
- Drivers in other languages implement the documented protocol. The spec lives at `docs/driver-protocol.md`.
- `cargo generate` template: `sqlr-driver-template`.
- **Conformance suite (`sqlr-driver-tests`):** a reusable test harness that runs against a real database in Docker. It checks connect, ping, browse, execute and streaming, `fetch_more`, cancellation, the type round-trip matrix and editing. Any driver that passes it can be packaged as a local driver file.

### 6.4 Driver Manager and local driver files

- **Install source:** only a local driver archive the user picks. sqlr does not browse or fetch
  a remote index, so there is no background update check and no remote code install.
- **Driver Manager UI:** search, install from a file, roll back between installed versions,
  remove versions or a driver, and restart a sidecar. When a user creates a connection for a
  database whose driver is not installed, the app opens the manager and points at
  **Install from file…**.
- **Install location:** `<user data dir>/drivers/<id>/<version>/`, with a `driver.toml`
  manifest and the executable. Several versions can coexist, so rolling back is instant.
- **Review:** an archive is inspected first; the user sees its id, version, size and SHA-256,
  and is told that the driver runs with their rights and receives connection passwords.
  Nothing is installed or started before an explicit confirmation.
- **Trust:** a file is never signed or verified by sqlr on the user's behalf. The review step
  is the trust boundary; install only archives from a source you trust.

## 7. Data flow

### 7.1 Query execution

```
UI ──Command::Execute{session, query}──▶ Engine ──▶ Driver
                                           │          │ Stream<ResultChunk>
UI ◀──request_repaint() (≤ ~60 Hz)──── ResultStore ◀──┘
 └─ reads only the visible window: store.window(rows, cols)
```

1. The UI sends the command and gets a `QueryId` back immediately. The tab shows the running state, elapsed time and rows received so far.
2. The driver yields chunks, which the engine appends to the result's `ResultStore`.
3. The grid formats cells **only for visible rows**, using a small LRU of formatted strings.

### 7.2 Chunk format

- Columnar and Arrow-like, but our own minimal format, because the `arrow` crate brings far more dependencies than we need.
- Per column: a typed vector (`i64`, `f64`, `bool`, decimal-as-string, timestamp, and so on) plus a validity bitmap. Variable-length data (text, bytes, JSON) is stored as an offsets array over a single byte arena.
- Target chunk size is about 1–4 MB.
- `DocChunk` stores documents in a compact binary encoding of the `Value` tree.
- The same bytes are used in-process, over the sidecar protocol and in spill files.

### 7.3 Incremental fetch

- **Default page size: 500 rows.**
- The page size can be changed in two places:
  - in Settings, as a global default;
  - from the "Fetch: 500 ▾" selector in each result's status bar, with presets 100 / 500 / 1000 / 5000 and a custom value.
- Scrolling near the end of the loaded rows triggers `fetch_more`, using the server-side cursors described in §5.6.
- **"Fetch all"** streams the rest of the result with visible progress and can be cancelled.

### 7.4 Memory limits

- There is a per-result budget (default 256 MB) and a global budget, both configurable.
- When a budget is exceeded, the oldest chunks are written to a temp file and read back via mmap. The format is already binary, so this costs almost nothing.
- Closing a result tab frees its buffers and temp files immediately.

### 7.5 Sort and filter

- **Complete result:** local sort and filter through a permutation index, without copying the data.
- **Partial result:** the UI offers to re-run the query on the server.
  - Relational: the `Dialect` wraps the query as `SELECT * FROM (<q>) t ORDER BY … / WHERE …`.
  - Document: native `sort` / `filter`.

### 7.6 Scripts

- The `Dialect` splits the script into statements, which run in order. Each statement that returns rows gets its own result tab. Other statements write to the Output tab.
- On error the behavior is configurable: **stop** (default) or **continue**.

### 7.7 Inline editing

- Edits go into a local **changeset** of modified cells, inserted rows and deleted rows. The grid highlights each kind with its own color.
- **Preview** shows the DML the `Dialect` generates. **Apply** runs it inside a transaction, followed by commit, or rollback on error.
- Rows are identified by the **primary key or a unique key**. If the result has neither, it is read-only and the UI explains why. The app never falls back to matching on all columns.
- Documents are identified by `_id` (MongoDB, Elasticsearch). Redis values are edited per key or field.
- Results from a connection in read-only mode are never editable.

### 7.8 Catalog cache

- The catalog is loaded lazily per schema and cached in memory and in SQLite. On startup the explorer and autocomplete work **immediately from the cache** while a background refresh runs.
- The cache entry for a connection is invalidated after a DDL statement runs on it, and on manual refresh.

### 7.9 Transactions and connection profiles

Every connection uses a **connection profile**. A profile decides how transactions are committed and how carefully the app guards the connection.

- **Profile fields:**
  - name and color (the color tints the toolbar, the tabs and the connection in the explorer);
  - **commit mode**: `Auto`, `Manual` or `Smart` (below);
  - confirm before running any statement that is not read-only;
  - confirm destructive statements (§9);
  - read-only;
  - idle transaction timeout: after N minutes with an open transaction and no activity, ask the user to commit or roll back (off by default).
- **Built-in profiles**, editable and resettable:
  - *Development*: `Auto`, no color, only destructive statements confirmed.
  - *Test*: `Smart`, amber.
  - *Production*: `Manual`, red, every non-read-only statement confirmed, destructive statements confirmed.
- **Read-only and savepoints (§2, item 18):** besides the profile's read-only flag, the status bar toggles read-only for the current session (enforced as below). In `Manual` and `Smart` mode, while a transaction is open, the user can create named savepoints and roll back to any of them.
- **Sessions:** with per-editor sessions on (default), commit mode, read-only and the open transaction belong to the editor tab's own session; the explorer, catalog loading and data tools use separate sessions.
- Users can add their own profiles. Profiles live in `profiles.toml` in the config directory; a connection stores its profile id in `connections.toml` (default: Development). The commit mode can also be switched for the current session from the toolbar without changing the profile.
- **Commit modes:**
  - **Auto:** every statement commits on its own. An explicit `BEGIN` typed by the user still opens a transaction, and sqlr keeps it open (the default behavior).
  - **Manual:** the engine opens a transaction before the first statement and keeps every following statement on that connection inside it until the user commits or rolls back. A new transaction starts with the next statement.
  - **Smart:** behaves like `Auto` until a statement that modifies data or schema runs (classified by the `Dialect`); that statement and every following one run in a transaction, as in `Manual`. After commit or rollback the session returns to `Auto`.
- **While a transaction is open:**
  - the toolbar shows **Commit** and **Rollback**, and the status bar shows "Transaction: N statements · since HH:MM";
  - the connection's explorer badge shows a pending-transaction mark;
  - closing the last tab, disconnecting or quitting asks: Commit, Roll back or Cancel; a disconnect or reconnect still waiting for the server 5 s after the answer is forced through;
  - a statement that fails inside the transaction leaves it open (PostgreSQL marks it aborted), and the status bar says that only Rollback is possible.
- Paged results read inside the open transaction (§7.3), so they see its uncommitted changes.
- **Read-only** is enforced by the server where possible (PostgreSQL `default_transaction_read_only`, MySQL `SET SESSION TRANSACTION READ ONLY`, SQLite read-only open) and by the client classifier as a second line.
- Drivers without the `transactions` capability (Redis, Elasticsearch, Mongo outside sessions) ignore the commit mode; color, confirmations and read-only still apply.

## 8. UI

### 8.1 Layout

- **Left:** a Connections / Explorer tree with every connection, organized in expandable folders and reorderable by drag and drop, and color-tagged.
- **Center top:** editor and object tabs (SQL files, open tables, Mongo collections, Redis databases).
- **Center bottom:** result panes (Result, Messages, Plan) plus Output, History, Log and Operations.
- **Right:** the value panel for the selected cell or document.
- **Toolbar:** the active connection and schema picker, Run (⌘↵), Stop and Refresh.
- **Status bar:** row count, timing, "Fetch: N ▾" and "Fetch all".
- All panels are docked through `egui_dock`. The arrangement is saved per window.

### 8.2 Widgets (`sqlr-ui`)

All widgets take data in and return action values. None of them calls the engine.

**`QueryEditor`**
- Text lives in a `ropey` buffer, with a per-line layout cache that is recomputed only for changed lines and visible lines. It must stay smooth on 10 MB files.
- Highlighting comes from a resumable line-state lexer chosen by dialect (SQL variants, Mongo shell/JSON, Query DSL, Redis commands). Each line stores its start state, and long lines keep in-line checkpoints. An edit re-lexes from that point until the state converges. The editor avoids parsing the entire document on each edit.
- tree-sitter parses only the statement(s) around the caret, for autocomplete context and statement detection. Regions larger than 64 KB are parsed off the UI thread. Grammar queries are adapted per consumer (e.g. `tree-sitter-sequel`'s Neovim-flavoured `#match?` patterns).
- Multi-cursor, grouped undo/redo, find and replace, auto-indent, bracket pairing, and toggle comment (⌘/).
- Context-aware autocomplete fed by the catalog cache: tables after `FROM` and `JOIN`, columns after `alias.`, keywords and functions from the `Dialect`. The popup is drawn by the widget itself.
- ⌘↵ runs the statement under the cursor or the selection. ⇧⌘↵ runs the script. Syntax errors with a position are underlined.

**`PlanView`**
- The visual explain plan: a summary of the plan's measured or estimated time, cost, rows, operator mix and warnings; a stacked hot path bar whose segments select a node; and the operators as collapsible cards connected as a tree. The selected node's metrics, filters, indexes, buffers and extras sit in an inspector beside the tree.
- The raw plan is one toggle away. A plan is a snapshot: when the statement at the caret changes, the pane offers a recompute instead of clearing it.

**`ResultGrid`**
- Virtualized on both rows and columns.
- Column widths are measured on a sample and can be resized. Columns can be frozen.
- Rectangular selection and spreadsheet-like keyboard navigation; ⌘-click adds or
  removes separate cell and row ranges.
- Copy as TSV, CSV, JSON, SQL `INSERT` or Markdown.
- Paste tab-separated text into the selection (a single value fills it); ⌘⌫ marks the
  selected rows deleted.
- Long values are truncated, and the full value appears in the value panel. Special types get dedicated rendering.
- Inline editing on double-click, with changeset highlighting.
- Also serves as the Table view for documents. Columns are the union of top-level keys of the first N documents (N = page size).

**`DocTree` and `JsonView`**
- Tree and JSON views of documents. Both are editable when the result is editable.

**`SchemaTree`**
- A generic tree driven by `browse(path)`.
- Children load lazily, with a loading state and inline errors on the affected node.
- Has a text filter.
- Context menu by node kind and capabilities: open data, copy name, generate SELECT, refresh.

**`ValuePanel`**
- A type-aware viewer and editor: text, formatted JSON, hex for binary, inline preview for binary that is an image.

**`KeyBrowser` and `KeyValueView`**
- Redis keys grouped by prefix with type badges, SCAN paging ("load more") and a match pattern.
- Per-type value views (string, hash, list, set, zset, stream) with editable TTL, size display and delete.
- A console tab for raw commands, with command history.

**`Palette`**
- ⌘P "open object" does fuzzy search over the catalog cache across all connections.
- ⌘⇧P "run command" covers app actions, history search and new connection.

### 8.3 Other app features

- **History:** a searchable panel of every executed query, filterable by connection, with timestamp, duration and outcome. Stored in SQLite.
- **Snippets:** named queries in folders, scoped to one connection or global.
- **Export:** CSV, JSON or SQL `INSERT`s, from a result or from a whole table or collection. Export streams and is cancellable.
- **Import:** CSV and JSON into a table or collection, with column mapping. SQL files open in the editor and run as scripts.
- **Themes:** curated light and dark themes, a configurable monospace font, and fully remappable shortcuts.
- **Operations monitor:** a dockable Operations panel, opened from the toolbar activity indicator or ⌘⇧P, lists every in-flight client operation — connect attempts, explorer/catalog reads, queries, fetches, scripts and background jobs — with its kind, connection, target, elapsed time, progress and state. Each cancellable entry has **Cancel**, and **Cancel all** asks once. Cancelling a connect drops the attempt and tears down any SSH tunnel or sidecar start it owned, so the row ends *Cancelled* instead of waiting for the timeout. Finished operations stay listed for a configurable time and then disappear; their outcome is still reported by the toast or result tab that owned them. The panel is client-side only: server sessions and locks remain in §8.4.

### 8.4 Object editors and administration

- **Object editor** is a tab per object with sub-tabs (Columns, Indexes, Constraints, Foreign keys, Triggers, Partitions, Permissions, DDL, Data, Properties). Edits accumulate as a pending change list, like inline data editing (§7.7). "Preview" shows the DDL the `Dialect` generates; "Apply" runs it in a transaction where the database supports transactional DDL, otherwise statement by statement with a clear report.
- **Create** dialogs for every object kind the driver declares, driven by the same forms.
- **DDL generation:** the `Dialect` (or the server, e.g. `pg_get_*def`) produces the DDL of any object; the explorer offers "Copy DDL", "Open DDL in editor" and "Generate SQL → SELECT/INSERT/UPDATE/DELETE/CREATE".
- **Drivers** declare which object kinds they can create, alter and drop through a new `ObjectEditing` capability descriptor, so the UI only shows what works.
- **Administration panels:** users and roles with grants (a grid of privileges per object); a session manager (active queries, wait events, lock graph, cancel and terminate); server properties, database size and table statistics. Destructive admin actions go through the connection profile's confirmations (§7.9).

### 8.5 Data tools

- **Dump and restore** run the database's native tools: `pg_dump`/`pg_restore`/`psql`, `mysqldump`/`mysql`, SQLite's online backup API, HANA `BACKUP DATA`/`RECOVER` on the server.
  - sqlr finds the tools on `PATH`, in Homebrew and Postgres.app locations, and in a per-connection "local client" path setting; a missing tool is reported with how to install it.
  - The wizard covers objects, format (plain, custom, directory, tar), compression, schema-only / data-only, clean/create, parallel jobs and encoding. It shows the exact command line before running.
  - The tool runs as a child process with the password passed through its environment or a temporary passfile, never on the command line. Its output streams into a live log; progress shows where the tool reports it; Cancel kills the process.
- **Data transfer** uses sqlr's own driver-based engine, between any source and target:
  - sources and targets: tables or a query on any connection; files in CSV, TSV, JSON, JSON Lines, Excel (.xlsx), Parquet and SQL INSERT;
  - steps: source → target (existing table or a new one, with DDL generated and types mapped across dialects) → column mapping with type conversions → options (batch size, commit per batch or once, truncate target, upsert by key, disable constraints/indexes during load);
  - data flows as chunks (§7.2) with bounded memory, is cancellable, and ends with a summary of rows read, written and rejected (rejected rows can be saved to a file).
- **Import and export** of §8.3 become presets of the transfer engine.
- **Saved tasks:** any wizard can be saved as a named task (stored in `sqlr.db`, secrets excluded) and run again from a Tasks panel, which also shows running and past runs with their logs.

### 8.6 Schema tools

- **Compare** two schemas or databases (same or different connections of the same engine): a tree of differences per object, a side-by-side DDL diff, and a generated migration script that opens in the editor. Objects can be excluded.
- **ER diagrams** for a schema or a selection of tables: automatic layout from foreign keys, drag to rearrange, layout saved per schema, export to PNG/SVG.
- **Visual explain plan:** `EXPLAIN (ANALYZE, FORMAT JSON)` and equivalents shown in the tab's Plan pane, on the tab's own session. The strip above the results carries an **Explain** split button (Explain, or Explain Analyze from its chevron). The pane summarizes execution and planning time, cost, rows and the operator mix, lists plain-language warnings (a row estimate off by 10×, a full scan over 10,000 rows, a node run 1,000 times, a spill to disk), and draws the plan as a hot path bar plus collapsible operator cards; the selected node's details sit beside it and the raw plan is a toggle away. Explain Analyze of a statement that writes asks inline first and runs inside a transaction that is rolled back; a running explain can be stopped. The plan is a snapshot, kept until the user recomputes it.
- **Test-data generator:** per-column generators (sequences, random ranges, names, emails, dates, foreign-key-aware picks) writing through the transfer engine.

### 8.7 Configurability

- A preferences window with search, organised into: General, Appearance (themes, fonts), Editor (font, tabs, autocomplete, formatting), Result grid (page size, formats, NULL display, colors), Data formats (date, time, number, binary display per type), Keymap (every shortcut remappable, with presets), Connections (defaults, profiles), Drivers (per-driver and per-connection properties), Tools (paths of native client tools).
- Settings are layered: global → connection profile → connection, each level overriding the previous.

## 9. Persistence and security

- **Connections:** stored as TOML in the user config directory, **without secrets**. They can be exported and imported for team sharing.
- **Secrets:** passwords, SSH passphrases and the MCP bearer token are stored in `secrets.vault` in the config directory. The file uses versioned AES-256-GCM authenticated encryption with a fresh random nonce for every write. A random 256-bit encryption key lives in the OS keychain through `keyring` (macOS Keychain, Windows Credential Manager, Linux Secret Service), scoped to the canonical config-directory path. The app shares one vault across the connection repository, engine, editor and MCP. Its first background operation reads the key; subsequent operations reuse it until shutdown, when the cached key is erased. The OS controls authorization dialogs; denial can be retried, but an unavailable keychain has no plaintext or master-password fallback. Moving the config directory requires its original keyring key; copying the encrypted file alone does not transfer credentials.
- **Vault integrity and concurrency:** background operations hold an in-process mutex and an OS file lock, reload the current file, and write through a private temporary file followed by sync and atomic replacement. Plaintext working buffers are erased after use; secrets never enter `connections.toml`, exports, logs or `Debug` output. A corrupt vault or missing/invalid key fails without replacing the stored data or generating a replacement key for an existing vault.
- **Credential migration:** old per-connection keychain entries and the old dedicated MCP token migrate lazily on first use. The encrypted value is persisted before deleting the old entry; pending cleanup survives interruption and is retried. Absence/deletion markers prevent stale legacy entries from restoring deleted secrets. Migration may still prompt for each old item once; after migration there is one master-key lookup per application session. Connection credentials are keyed by connection id and field, independently of the MCP token, so rotating the token never changes a connection password.
- **SQLite (`sqlr-store`):** history, snippets, catalog cache and UI layout.
- **SSH tunnels:** `russh` with password, key and agent authentication, plus one jump host.
- **TLS:** `rustls` with system roots, optional custom CA, client certificates and a verify-mode setting.
- **Production safeguards** come from the connection profile (§7.9):
  - the profile color on the toolbar, the tabs and the explorer;
  - an optional read-only mode;
  - a confirmation dialog before destructive statements: `DELETE` or `UPDATE` without `WHERE`, `DROP`, `TRUNCATE`, `FLUSHDB`/`FLUSHALL`.
- **Drivers:** signature verification as described in §6.4.
- **No telemetry.** Diagnostics are local only, with no network requests. Help → Diagnostics previews, copies and saves bounded JSON bundles; from the review window, **Open GitHub issue** hands the report to the browser's public new-issue page, which pre-fills the whole bundle when it fits the URL and otherwise asks for one paste of the bundle copied to the clipboard. sqlr itself sends nothing and the person decides whether to submit. Secrets and vault contents are structurally excluded; SQL defaults on and can be omitted globally with `diagnostics.include_sql`. Reports never mask SQL literals automatically. Logs and diagnostics directories are owner-only.

## 10. Error handling

- `DriverError { kind, message, position?, hint? }`. Its `kind` is one of `Connection`, `Auth`, `Syntax`, `Timeout`, `Cancelled`, `Permission`, `Unsupported` or `Protocol`.
- How the UI presents each kind:
  - `Syntax` with a position → underline in the editor;
  - `Connection` → banner with "Reconnect";
  - `Permission` → inline on the explorer node;
  - all others → error in the result tab.
- **No driver failure may crash the UI.**
  - Panics in engine command tasks are caught and converted to failed operations plus an `engine_panic` incident; other operations continue.
  - A sidecar crash fails only its own connections.
- Logging uses `tracing` to stderr, daily files (newest fourteen retained) and a bounded in-memory ring. Every line carries the session identifier. Expected failures are logged once at settlement; `error!` means an unexpected incident.
- Incidents are standalone JSON under `diagnostics/` in the data directory, independent of SQLite, settings and the vault; retention is twenty files or thirty days. Startup failures produce an incident and a native dialog.
- The global panic hook writes a report before exiting. The next launch offers a non-blocking banner with Open report, Copy, Report on GitHub and persistent Dismiss. A session marker and lock distinguish clean exit, unclean shutdown and a live second instance.
- Help → Diagnostics and Report a Problem preview bundles before copying, saving or opening a GitHub issue; result errors have Copy details. Bundles are capped at 512 KiB with explicit truncation markers. The log level is session-only and `RUST_LOG` takes precedence.
- Release binaries remain stripped. Crash reports contain messages and source locations, without backtraces or symbols; native crashes are detected as unclean shutdowns on the next launch.

## 11. Testing

1. **Unit tests** in `core` and `engine`: chunk encode/decode, statement splitting per dialect, DML generation, the changeset, Mongo shell-syntax parsing and the destructive-statement classifier.
2. **Driver conformance suite** (§6.3), run against the fixed PostgreSQL, MySQL, MariaDB, MongoDB, Redis, Elasticsearch, SQL Server and Oracle test services in CI. HANA runs only where a test instance is available.
3. **Widget tests** with `egui_kittest`: simulated interaction plus snapshot tests for `QueryEditor`, `ResultGrid`, `SchemaTree` and `KeyBrowser`.
4. **Fuzzing** of the sidecar protocol decoder and the chunk decoder with `cargo-fuzz`.
5. **Performance budgets in CI:** cold start time, idle RAM, binary size, and grid frame time on a 1M-row fixture. The build fails if any target in §1.3 regresses.

## 12. Distribution

- **macOS:** signed and notarized `.dmg` (requires an Apple Developer account) and a Homebrew cask.
- **Windows:** `.msi` and winget.
- **Linux:** AppImage, `.deb` and Flatpak.
- **Updates:** once a day the app asks GitHub Releases whether a stable release newer than
  the running one exists; **Settings → General** turns the check off. It verifies the
  archive against the release's `SHA256SUMS.txt` and asks before replacing anything. On
  macOS it replaces the `.app` bundle (asking for the administrator password when needed),
  on Linux the binary when its folder is writable; Windows opens the release page. Nothing
  is downloaded until the person chooses Install, and a check that cannot reach GitHub is
  silent and retried on the next launch. **Settings → General** and **Help** can run the
  same check on demand; an explicit check reports its outcome.
- **CI:** GitHub Actions with a matrix over macOS arm64/x64, Windows x64 and Linux x64.

## 13. Open risks

| Risk | Mitigation |
|---|---|
| `QueryEditor` syntax layer at large file sizes | The editor must keep interactions responsive on large files; its per-line lexer and layout cache limit work to changed regions. |
| `hdbconnect` has essentially one maintainer and a slow release cadence | The driver runs as a sidecar, so a `SAP/go-hdb` sidecar can replace it transparently. Upstream the PEM fix. Validate the driver against native HANA and HANA Cloud environments. |
| GPU-dependent rendering on old or virtualized machines | eframe's glow (OpenGL) fallback, selectable in settings. |
| Mongo shell syntax expectations beyond our subset | Document the supported subset clearly, and provide a raw-command mode (`runCommand` JSON) for anything else. |

## 14. Agent access and the integrated terminal

sqlr can expose a native MCP server while it runs. It is disabled by default, binds only
`127.0.0.1`, requires a keychain-backed bearer token on every request, validates `Host`
and `Origin`, publishes a token-free `mcp.json`, and exposes MCP tools only. The server
stops and removes its discovery file when disabled or when sqlr quits.

Each connection has an **agent access** level, `None` (the default), `Read` or `Write`,
chosen only by the person in the connection dialog or the explorer. `Read` forces the
agent's engine session read-only; `Write` still follows the connection profile's commit
mode, read-only flag and confirmations. Exposed connections are marked in the explorer.
Connections open lazily and visibly; the agent has its own engine session per connection
and MCP session, isolated from editor transactions, and its open transactions are rolled
back when the MCP session ends. There is no connect or disconnect tool.

The tool surface covers a guide, catalog browsing and searching, schemas, one-statement
queries with paged typed results, editor-tab read/edit/run with revision checks, snippets,
and configuration reads/writes. Connection values and secrets are write-only. Driver
installs, MCP settings, application updates and `terminal.shell` are human-only; safety
flags are tighten-only, including profile changes, setting resets and transport-security
driver fields. Destructive statements and configuration/snippet deletes use the same
in-app approval dialog as human runs, with a 60-second deny-by-default timeout and a
structured `denied`/`timeout` error.

The integrated terminal is a real PTY with a login shell, opened with `Ctrl+Backtick`,
kept running when hidden, and rendered with the app theme and monospace font (ANSI, wide
characters, alternate screen, resize, selection, bracketed paste). It is human-only and
never exposed to MCP; its output and shell command are never logged, no MCP token is
injected, and its process group is killed on quit. Existing performance budgets are
unchanged when MCP and the terminal are disabled or unopened.
