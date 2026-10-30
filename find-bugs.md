# Bug hunt

Cycle 1 completed on 2026-10-30: all 19 sections checked. The full workspace suite
(container profile, `make test`) ran 2487 tests: 2487 passed, 22 skipped, 1 flaky
(`sqlr-app::smoke history_context_menu_saves_the_query_as_a_snippet` timed out on the
first try and passed on retry; worth a deterministic look in this cycle).

## Cycle 2 sections:
[x] sqlr-core: values, encoding and documents (`value`, `encode`, `read`, `rows`, `chunk`, `doc*`, `formats`)
[x] sqlr-core: SQL and text utilities (`classify`, `syntax`, `format_sql`, `generate`, `schema_change`, `compare`, `datagen`, `transfer`, `url`, `wire`, `es_request`, `mongo_shell`, `er`)
[x] sqlr-store: persistence and secrets (connections, vault, settings, history, snippets, keymap, recents, profiles, layered)
[ ] sqlr-engine: sessions, transactions and results (`engine`, `session`, `tx`, `script`, `apply`, `result`, `result_memory`, `result_spill`, `job`)
[x] sqlr-engine: transfer and imports (`transfer/*`)
[x] sqlr-sidecar: protocol, client and process
[x] sqlr-registry: driver install, index and verification
[x] sqlr-mcp: server, policy, tokens and results
[x] sqlr-terminal: PTY, screen, input and tabs
[ ] drivers: PostgreSQL
[x] drivers: MySQL / MariaDB
[x] drivers: SQLite
[x] drivers: Redis
[x] drivers: SQL Server / Oracle
[x] drivers: Elasticsearch / Snowflake / ClickHouse / JDBC
[x] sidecar drivers: MongoDB / DuckDB / HANA
[x] sqlr-app: application logic (workers, actions, messages, cli, tabs, pending)
[x] sqlr-ui: grid and editor
[x] sqlr-ui: forms, trees, dialogs and keymap

## Cycle 1 findings:
# sqlr-core: values, encoding and documents
Fixed:
- A UTF-8 BOM split across `BufRead` buffer reads was not skipped (a one-byte buffer left `\u{feff}` in the first header or made JSON fail). The BOM is now gathered across reads and non-BOM bytes are chained back into the reader.
- A headerless single-column CSV lost leading blank rows (`"\n1\n"` decoded as one row): a leading NULL row exported by sqlr could not be read back. Blank records are now held until the width is known; blank lines before a header are skipped as preamble.
- CSV/TSV export with display formats on prefixed formatted numbers with the spreadsheet apostrophe (`'-1,234`), turning negative numbers into text. The escape now applies only to cells that were text in the result.
- Number grouping was not applied to text-backed numeric columns (e.g. DuckDB `HUGEINT`, `DisplayHint::Number`). Grouping now covers both `Number` and `Decimal` text hints.
- `format_iso_date` emitted unparseable years outside 0000–9999 (`-001-…`, `10000-…`). It now writes ISO 8601 expanded years (`-0001`, `+10000`), `parse_iso_date` accepts them, and its arithmetic is checked against overflow.
- Legacy Extended JSON `{"$type": …, "$binary": …}` depended on key order and silently stayed a document. Both member orders are accepted.
- `{"$date": 1e300}` saturated to `DateTime(i64::MAX)` instead of being rejected; out-of-range integral doubles are now an `InvalidWrapper` error.
- `docs_to_rows` dropped documents with no fields (`[{}]` rendered as zero rows). Empty documents now still produce their rows.

Tests added in `read.rs`, `encode.rs`, `formats.rs`, `doc.rs` and `doc_rows.rs`; `cargo test -p sqlr-core` and `cargo clippy -p sqlr-core --all-targets` pass.

# sqlr-core: SQL and text utilities
Fixed:
- SQLite/DuckDB file URLs built by `connection_url` did not encode reserved path characters and turned relative SQLite paths into absolute ones; such URLs parsed back to a different file (or failed). Paths are now encoded per segment and relative paths stay relative.
- `redact_url` leaked a password in malformed URLs without `://` (for example `postgresql //bob:secret@h`). The authority-like part is now redacted even without a scheme separator, and `%+1`/`%-1` are rejected as invalid percent escapes.
- `classify_script` returned Read for `EXPLAIN SELECT 1\nDELETE FROM t`: a hidden second statement bypassed read-only enforcement and destructive confirmations. It now scans the batch after the planned statement and catches the hidden write.
- A table rename in the object editor dropped its triggers because the editor leaves `triggers[].table` stale; `diff_table` compared triggers by the table reference. Triggers are now compared by name and definition, ignoring the table reference.
- Generate SQL produced `SET` with no assignment for a table whose only columns are generated/identity and has no row key; it now returns a clear error.
- Data generator arithmetic overflowed (debug panic, release wrap) for extreme `Sequence` or `DateRange` values; it now returns `RangeExhausted`. Type matching no longer treats `interval`/`point` as integers (`contains("int")`).
- Compare reported a `Changed` object with no changes when only column order differed, so the diff reappeared on every compare; such normalized differences are now ignored. Quoted type names keep their case (`"MyType"` is not lowercased), and `requalify`/`collapse_ws` leave dollar-quoted strings and comments untouched.
- ER diagrams matched foreign-key targets by table name only, drawing an edge to a same-named table in another schema; targets now match schema and name.
- The SQL formatter removed the newline between adjacent string literals when a comment sat between them, producing a syntax error; the newline is preserved.
- Type mapping for transfer targets other than PostgreSQL/MySQL/SQLite used the SQLite branch (e.g. `bigint` became 32-bit `integer` on SQL Server, `text` was emitted for Oracle). Added mappings for SQL Server, Oracle, DuckDB, ClickHouse, Snowflake, HANA and JDBC; unknown targets keep the source type with a warning.
- A single-action `_bulk` request was sent as `application/json` because `is_ndjson` only accepted arrays; it is now NDJSON and the Elasticsearch driver wraps the single action as one NDJSON line.

Tests added across the affected modules plus `tests/format_sql.rs`; `cargo test -p sqlr-core`, `cargo test -p sqlr-driver-elastic --lib` and clippy for both crates pass.

# sqlr-store: persistence and secrets
Fixed:
- Startup `trim_history` deleted history rows and snippets containing `passwd`, although the write-time rule documented in the query-editor spec (and implemented by `may_hold_secret`) only excludes `pwd`, `password`, `api_key` and `identified by`. Such entries were accepted and then silently lost on the next launch. The trim now uses the same rule.
- `ConnectionRepo::delete` wrote `connections.toml` before deleting the vault entries. If the keychain/vault delete failed, the connection was already gone (no retry) and its secrets stayed unreachable under an unknown id. Secrets are now deleted first, so a failure leaves the config intact and retryable.

Tests added in `crates/sqlr-store/tests/db.rs` and `tests/store.rs`; `cargo nextest run -p sqlr-store` and clippy pass.

# sqlr-engine: sessions, transactions and results
Fixed:
- A failed transaction command left the tracked state wrong: after a failed `COMMIT` on a PostgreSQL-like driver the transaction is gone, and a failed `SAVEPOINT`/`ROLLBACK TO` aborts it, but the engine kept `Open` (and `tx_conn`, savepoints and `wrote`). The error path now closes the transaction for a failed commit/rollback and marks it `Failed` for an aborting savepoint command.
- A no-op `COMMIT`/`ROLLBACK`/`SAVEPOINT`/`ROLLBACK TO` closed the session's streaming result before checking that it was a no-op, so the remaining pages became unreachable (`cut_short`). The result is now closed only after the command is known to run.
- `apply_changeset`, `import` and `export` refused a session in `Failed` instead of reconnecting it; they now use `live_or_reopen`, like `execute_in`.
- `connect` with an unknown driver removed the connection entry but left its tab/agent sessions running on the old connection; they are now retired too.
- A read-only switch requested while a session was still `Connecting` was lost when that connect failed. The request is now remembered for the next reconnect.

Open for the next cycle (needs a deterministic test): a later query marks earlier results `superseded` before its execute claims the stream; if it is closed first, the earlier result can still be cut short.

Tests added in `crates/sqlr-engine/tests/tx.rs` and `tests/apply.rs`, with a small `connect_delay` test hook in `sqlr-testkit`; targeted integration tests and clippy pass.

# sqlr-engine: transfer and imports
Fixed:
- With `skip_bad_rows` on a collection target, a failed `insertMany`/`_bulk` was retried document by document. MongoDB's ordered insert and Elasticsearch's bulk can apply a prefix before failing, so the retry inserted that prefix twice (or reported it as duplicate keys). Documents are now inserted one at a time from the start when skipping bad rows.
- The Redis 64 MB value limit was checked only between pages, so a value delivered whole in one page (or the final page) was never measured. The size is now checked after the first page and after every append.
- XLSX export rounded integers above 2^53 and silently truncated text over Excel's 32,767-character limit. Large integers are written as exact text, and a too-long cell now fails with a clear error instead of losing data.
- XLSX serial dates before 1900-03-01 were one day early because of Excel's fictitious 1900-02-29; serials 1..59 now map to the dates Excel shows.
- `utc_naive` normalized invalid dates and times (`2024-02-31`, `25:00`, `+24:00`, a bare `+`, a non-numeric fraction) into a different valid timestamp; invalid input now comes back unchanged.
- `max_rejected` allowed one rejection more than the configured maximum (`Some(1)` stopped after two). It now stops at the configured count and the message uses the singular for one row.
- A workbook/Parquet export cancelled before the first batch (or after the last one) still renamed the partial file and reported `Exported`; the cancel is now checked before and after the batch loop.

Tests added in the transfer modules and `tests/transfer_engine.rs` / `tests/nosql_transfer.rs`; targeted engine tests and clippy pass.

# sqlr-sidecar: protocol, client and process
Fixed:
- An exhausted row stream was left in the server's stream map: the client sends no `CloseStream` after `exhausted: true`, so every fully read query leaked its cursor until disconnect. The server now closes and removes it.
- `connect_database` had no `ConnectGuard`: a cancelled future could leave the derived connection open on the server. It now uses the same guard as `connect`.
- The idle timer only looked at `conns`, so a connect started just before the deadline was shut down mid-handshake. It now also requires `connecting == 0`.
- The client returned more rows than `max_rows` when a chunk was split into several frames and later frames were buffered. Buffered row chunks are now split to the requested maximum, like document chunks.
- A slow `Execute` that finished after its connection was disconnected inserted a stream nobody could close. The server now checks the connection still exists and closes the stream otherwise.
- Cancelling a statement counted every in-flight call (`Browse`, `Catalog`, `Ping`, …) and could kill the whole driver for an unrelated slow catalog load. Only statement calls and fetches now keep the cancel waiting.

- `Shutdown` aborted only connecting tasks and then waited for in-flight `Execute`/`Fetch` tasks, so a long statement delayed process exit. All request tasks are now aborted before streams are closed and the writer is awaited.

Open for the next cycle: a nonconforming server answering `FetchDone { exhausted: false }` with no chunk makes the client report exhaustion.

Tests added in `crates/sqlr-sidecar/tests/server.rs` and `tests/sidecar.rs`, plus the echo test driver's slow-connect/slow-browse hooks; the sidecar test suites and clippy pass.

# sqlr-registry: driver install, index and verification
Fixed:
- Removing one version could delete the whole driver: when the remaining active version had a missing/unreadable `manifest.json` or `driver.toml`, `installed_one` returned `Err` and `remove` treated it like "nothing left", running `remove_dir_all` on the driver directory. Only `Ok(None)` now removes the whole directory; errors propagate.
- Redirects were allowed to any URL on the same host, so an HTTPS index or artifact could be downgraded to HTTP or moved to another port on the same host. Same-host redirects now require the same scheme and effective port.
- `activate` checked `version_dir(id, version)/driver.toml` with a raw string, so `./1.0.0` or `../…` could pass the check and then be ignored by `installed_one` (silently activating the newest). The version must now parse as a bare semver.
- `Fetcher` stored a refused redirect in one shared slot, so concurrent downloads could report another request's redirect refusal for an unrelated error. Refusals are keyed by the request's original URL.
- Versions that differ only in build metadata (`1.0.0+b1`, `1.0.0+b2`) sorted equal and the active fallback depended on directory order. Ties are broken by the full version string.

Tests added in `crates/sqlr-registry/tests/registry.rs` and `src/download.rs`; the registry test suites and clippy pass.

# sqlr-mcp: server, policy, tokens and results
Fixed:
- `config_write` with `"profile": null` silently downgraded a Production or custom read-only profile to Development. Clearing now resolves to Development and passes the same strictness check as naming it.
- `ssl_mode` was not treated as a transport-security field, so an agent could switch PostgreSQL/MySQL from `verify-full` to `disable` or remove the field. It is now tighten-only, with the TLS modes ranked from `disable` to `verify-full`.
- A client-supplied `Mcp-Session-Id` was trusted: any unknown id created a session entry (and a fresh tab budget) per call. Only ids created by the capped session manager are accepted; other calls share the stateless context.
- `offset + limit` could overflow for a huge `offset`. The offset is validated against `usize` and the page bounds use saturating addition.
- Local Redis console commands (`SELECT`, `WATCH`, `UNWATCH`) were reported as “does not parse” after running; the console is now polled for their output, and the console is closed on timeout too.
- Retained results were looked up by id only: another MCP session could read or close them. Lookups, touches and closes now require the owner session.
- The first row/document bypassed `MAX_PAGE_BYTES`, so a single 1 MiB cell was returned whole with `truncated:false`. The page budget now applies to the first item too.
- `wait_ready`/`wait_docs_ready` slept with `std::thread::sleep` inside async handlers and could stall all MCP traffic; they now await `tokio::time::sleep`, and the page functions are async.

Tests added in `crates/sqlr-mcp/tests/protocol.rs` and policy unit tests; the full sqlr-mcp suite and clippy pass.

# sqlr-terminal: PTY, screen, input and tabs
Fixed:
- Shift+Tab was sent as a plain Tab, so readline/less never received backtab; it now sends `CSI Z`.
- Copying a selection containing a wide character inserted a space at its continuation cell (`界e` became `界 e`). Continuation cells are skipped and a selection starting on one snaps to the wide character.
- Erased or blank cells with a background colour were not painted, so `CSI 2J`/`K` backgrounds and selection highlights over blank cells disappeared. The background is now painted before the empty glyph is skipped.
- Mouse and selection cell mapping averaged the canvas width over the column count, which drifts from the painted glyph width when the width is not an exact multiple; clicks and SGR reports could target the neighbouring cell. The painted `char_width`/`row_height` are now used.

Tests added in `input.rs`, `screen.rs` and `widget.rs`; `cargo test -p sqlr-terminal` (47 tests) and clippy pass. The `input.rs`/`screen.rs` changes landed in commit f351b62 with the MCP work; the widget changes are in the following terminal commit.

# drivers: PostgreSQL
Fixed:
- `EXPLAIN` refused any SQL containing the substring `$1`, even inside a string literal, dollar-quoted text or a comment. Parameters are now detected by lexing (`TokenKind::Param`).
- Parenthesized queries (`(SELECT …) UNION (SELECT …)`, `(VALUES …)`) were not recognized as cursor queries because only the first token was inspected; they now skip leading parentheses and use the cursor path.
- A `WITH … INSERT/UPDATE/DELETE … RETURNING` with parameters was decoded from binary instead of the cursor's typed text path, so returned values could change type. A `WITH` statement is now wrapped when its main statement is a data change, falling back to binary only for data-modifying CTEs the wrapper cannot nest.

Tests added in `dialect.rs` and `execute.rs`, plus a container test in `tests/execute.rs`; 34 unit tests and clippy pass.

# drivers: MySQL / MariaDB
Fixed:
- A generated column's `NOT NULL` was dropped by `column_definition`, so a load → `CREATE` → load round trip changed it to nullable. MySQL now writes the nullability after `STORED`/`VIRTUAL`; MariaDB still omits it because its grammar rejects it (MDEV-10964).
- Spatial indexes reported by `information_schema.STATISTICS` as `RTREE` could not be recreated (`unknown index type 'RTREE'`). Both `RTREE` and `SPATIAL` now emit `SPATIAL`.
- `MyDialect` recorded `NO_BACKSLASH_ESCAPES` but `syntax()` always returned the backslash-escaping syntax, so `SELECT 'C:\'; DELETE FROM t` was lexed as one unterminated string and the hidden `DELETE` was classified `Read`. A second cached syntax without backslash escapes is now used for those sessions.

Tests added in `ddl_gen.rs`, `dialect.rs`; 31 unit tests and clippy pass.

# drivers: SQLite
Fixed:
- A result cut by a newer statement answered `Ok(None)` while the newer result was still open, so the cut looked complete. Both fetch paths now return the shared `Cancelled` “ended by a newer statement” error.
- Adding a trigger while renaming a table used the trigger's stale table reference, so SQLite created it on the old (now missing) name. The native `ALTER TABLE` path now creates the trigger on the renamed table.
- The schema snapshot included a virtual table's shadow tables (`docs_data`, `docs_idx`, …) as ordinary objects; a compare/migration could create or drop them and corrupt the FTS table. `pragma_table_list` shadow entries are excluded.
- SQLite's `unrecognized token` parse error was classified as `Other`; it is now `Syntax`, with its position.

Tests added in `src/errors.rs`, `src/snapshot.rs`, `tests/objects.rs` and `tests/sqlite.rs`; 18 unit + 60 integration tests and clippy pass.

# drivers: Redis
Fixed:
- RESP3 big numbers were rendered with `Debug` (`[49, 50, …]`) instead of their digits; the bytes are now decoded as text.
- Destructive-option detection scanned every argument, so a `MIGRATE` key/password named `COPY` was treated as non-destructive, while `COPY a REPLACE` and `RESTORE REPLACE 0 payload` were falsely destructive. Options are now read from their real positions, skipping `AUTH`/`AUTH2` arguments and stopping at `KEYS`.
- `LCS` and `ZINTERCARD` were missing from the read-only command list, and `SYNC`/`PSYNC` were allowed in the console (they take over the connection). Added to reads/refusals.
- An empty command inside a `MULTI` block encoded `*0`, which Redis ignores, leaving the console waiting forever. Empty commands are now refused with `ERR empty command`.

Tests added in `classify.rs`, `errors.rs` and `connect.rs`; 21 unit tests and clippy pass.

# drivers: SQL Server / Oracle
Fixed:
- Oracle decoded a NULL CLOB/BLOB (`LobValue::Null`) as empty text/bytes, losing the distinction between NULL and empty. It now decodes to `Value::Null`.
- Oracle TIMESTAMP WITH TIME ZONE lost its `+00:00` zone (and printed `-00:30` as `+00:30` because only the hour offset was checked). The zone is kept for a TimestampTz column and the sign considers both offset components.
- SQL Server appended the `MERGE` terminator after a trailing line comment (`MERGE … -- note;`), leaving the statement unterminated. The `;` is now on its own line.

Tests added in `types.rs` and `execute.rs`; 20 SQL Server + 31 Oracle unit tests and clippy pass.

# drivers: Elasticsearch / Snowflake / ClickHouse / JDBC
Fixed:
- Elasticsearch `_update_by_query`/`_delete_by_query`/`_reindex` responses with a non-empty `failures` list were reported as successful counts; the first failure is now an error naming its type and reason.
- Snowflake materialized views were listed and catalogued as ordinary tables; `MATERIALIZED VIEW` now maps to `MaterializedView` in both the explorer and the catalog.
- Snowflake SQLSTATE classes `08` (connection) and `0A` (unsupported) fell through to `Other`; they now map to `Connection`/`Unsupported`.
- The JDBC Java-version parser took the first quoted string, so a `Picked up JAVA_TOOL_OPTIONS: "…"` banner made it miss the version; it now reads the string after `version "`.
- ClickHouse needed no change in this pass.

Tests added in `write.rs`, `catalog.rs`, `tests.rs` and `java.rs`; 14 Elastic + 27 Snowflake + 11 JDBC unit tests and clippy pass.

# sidecar drivers: MongoDB / DuckDB / HANA
Fixed:
- DuckDB `BIT` and `BIGNUM` columns were mapped to `Text` but delivered as `Blob`, so `ChunkBuilder` rejected every row of the result. `BIT` is now decoded from DuckDB's padding-byte storage into its 1/0 text and `BIGNUM` is shown as opaque binary.
- HANA identity columns were all described as generated and none as automatic: `BY DEFAULT AS IDENTITY` became read-only in the grid and pasted rows kept their ids. Only `ALWAYS AS …` is generated; both identity kinds are automatic.
- HANA syntax-error positions are 1-based but `DriverError.position` is 0-based, so the editor highlighted one character to the right. The position is converted and clamped.
- `find().limit(NumberLong("-9223372036854775808"))` panicked negating `i64::MIN` (and wrapped in release). The limit is negated with `checked_neg` and an unrepresentable magnitude is a syntax error.

Tests added in `types.rs`, `catalog.rs`, `errors.rs` and `query.rs`; 8 Mongo + 1 DuckDB + 13 HANA unit tests, 14 DuckDB native tests and clippy pass.

# sqlr-app: application logic
Fixed:
- The next savepoint name started at `savepoints.len() + 1`, so an existing `sp2` made the button create `sp3` instead of the free `sp1`. Names now start at `sp1` and skip only those in use.
- Reopening an already-open DDL/object/ER/admin tab in another dock group only marked it active inside its leaf; the dock's focused leaf stayed elsewhere, so the tab did not come forward. A `focus_path` helper now also focuses the leaf, and all “focus existing tab” sites use it.
- Clicking a recent document connection connected it but opened no tab: `open_recent` sent documents through the SQL editor path, which rejects NoSQL connections. Document connections now open a document tab.

Tests added in `transaction.rs`, `ddl_tab.rs` and `welcome.rs`; 242 lib tests and clippy pass.

# sqlr-ui: grid and editor
Fixed:
- `parse_hex` sliced the digit string at byte pairs without checking ASCII, so `0x€€` or `0x😀` panicked on a char boundary. It now rejects non-hex digits before slicing.
- `SyntaxLayer::statements` could slice `text[s..range.end]` with `s > range.end` when a selection started after a statement on the same line (`select 1; select 2`), panicking. Ranges ending before the selection are skipped.
- Soft wrap counted the hidden `\r` of CRLF as a visible character, wrapping a line one row early and overstating the total. `\r` before `\n` is skipped.
- A document table whose first page was empty recorded `chunks_seen` and never picked up the columns of later non-empty pages, leaving it blank. It now rebuilds when the first non-empty page arrives.

Tests added in `grid/edit.rs`, `grid/doc_table.rs`, `editor/syntax.rs` and `editor/wrap.rs`; 167 lib + targeted editor/grid/docs integration tests and clippy pass.

# sqlr-ui: forms, trees, dialogs and keymap
Fixed:
- Revealing a plan hot-path node did not open the root recorded by “Collapse all” (the loop started at 1 instead of 0), so the tree stayed collapsed.
- A deadlock cycle with no non-waiting blocker rendered as “nobody waits”; the lock tree now starts from the first edge of a cycle.
- Snippet folders with the same name under different parents shared an egui id, so only one row worked and clicks toggled the wrong folder. The full folder path is now the id.
- XML pretty-printing ended a tag at the first `>`, even inside a quoted attribute (`<a b="x>y">`), splitting valid XML. A quote-aware tag scanner is used.

Tests added in `tests/plan_view.rs`, `tests/admin.rs`, `tests/library.rs` and `src/value_panel.rs`; 166 unit + targeted integration tests and clippy pass.

## Cycle 2 findings:

# sqlr-core: values, encoding and documents
Fixed:
- A headerless CSV/TSV whose first non-blank record was wider than one column still emitted every leading blank line as a one-cell row (and `decode` padded it to an all-NULL row). Those blanks are file preamble once the width is known to be greater than one, like blank lines after it; they are now dropped, while a genuinely one-column file still keeps them.

Tests added in `read.rs`; 249 sqlr-core lib tests and clippy pass.

# sqlr-mcp: server, policy, tokens and results
Fixed:
- MCP document and Redis reply values collapsed every non-finite double to the string `"NaN"`: `Infinity` and `-Infinity` were indistinguishable from NaN. They now map to `"Infinity"`/`"-Infinity"`/`"NaN"` like tabular cells do (spec: agents-and-terminal §JSON numbers).
- A colour setting accepted an array with a fourth invalid element: `[1,2,3,256]` dropped the 256 and stored `[1,2,3]`. The parser now requires exactly three byte channels and rejects any invalid one.

Tests added in `results.rs` and `policy.rs`; 26 lib + 29 protocol tests and clippy pass.

# sqlr-terminal: PTY, screen, input and tabs
Fixed:
- The terminal's Clear action rebuilt the vt100 parser with zero scrollback, so after clearing the screen the configured scrollback stopped being kept for the rest of the session. The emulator now remembers the configured size and recreates the parser with it.

Test added in `screen.rs`; 48 terminal lib tests and clippy pass.

# drivers: SQLite
Fixed:
- The explorer listed an FTS/virtual table's shadow tables (`docs_data`, `docs_idx`, …) as ordinary tables, although the schema snapshot excludes them. A user could edit or drop the module's storage from the tree. `browse` now filters `pragma_table_list` shadow entries too.
- A table whose AUTOINCREMENT primary key was changed to a non-`INTEGER` type generated `BIGINT PRIMARY KEY AUTOINCREMENT`, which SQLite rejects on apply, instead of the generator's clear "needs a one-column INTEGER primary key" error. The type is now checked.

Tests added in `browse.rs` and `ddl_gen.rs`; 20 unit + 60 integration tests and clippy pass.

# sqlr-core: SQL and text utilities
Fixed:
- `classify_script` flagged a read-only table-valued `PRAGMA` (`PRAGMA table_info(t)`, `PRAGMA main.table_list(t)`) as `Destructive("PRAGMA in a batch of statements")` because the batch scan re-examined the first statement's own verb. The first word is now exempt; a `PRAGMA` hidden after another statement (or inside `BEGIN … END`) is still caught.

Test added in `classify.rs`; 249 sqlr-core lib tests and clippy pass.

# sqlr-store: persistence and secrets
No new bug found in this cycle. Reviewed `connections` (import, save, folder moves), `vault`/`secrets`, `settings`, `history`, `snippets`, `keymap`, `recent`/`recent_objects`, `profiles`, `layered`, `catalog`, `db` migration/trim and the client-migration import/export paths; existing tests and clippy pass.

# sqlr-engine: transfer and imports
No new bug found in this cycle. Reviewed `xlsx`, `parquet`, `convert` (UTC/date arithmetic), `pipeline` cancellation, `write_table` rejected rows and `clipboard`; existing targeted tests and clippy pass.

# sqlr-sidecar: protocol, client and process
No new bug found beyond the open item already recorded: a nonconforming server that answers `FetchDone { exhausted: false }` with no chunk still makes the client report exhaustion. `frame`, `messages`, `client`, `process`, `server` and `dialect` were re-read; the shutdown task-abort fix remains in place.

# sqlr-registry: driver install, index and verification
No new bug found in this cycle. Reviewed `verify`, `install` archive extraction/limits, `index` parsing/version order, `download`, `manual` and `release`; existing tests and clippy pass.

# drivers: MySQL / MariaDB
No new bug found in this cycle. Reviewed `types` (both protocols, BIT/fraction), `execute` streaming/cleanup, `dialect`/`syntax` and `ddl_gen`; 31 unit tests and clippy pass.

# drivers: Redis
No new bug found in this cycle. Reviewed `values` (streams/scan/score parsing), `classify`, `console` and `errors`; 21 unit tests and clippy pass.

# drivers: SQL Server / Oracle
No new bug found in this cycle. Reviewed SQL Server `execute` (rowsets, MERGE, cleanup) and Oracle value/LOB/timestamp decoding; unit tests and clippy pass.

# drivers: Elasticsearch / Snowflake / ClickHouse / JDBC
No new bug found in this cycle. Reviewed Elasticsearch `write` bulk/by-query responses, Snowflake `protocol` partitions/data and SQLSTATE mapping, ClickHouse `stream` decoding, and the JDBC Java discovery; unit tests and clippy pass.

# sidecar drivers: MongoDB / DuckDB / HANA
No new bug found in this cycle. Reviewed DuckDB `types` (HUGEINT/UUID/BIT/TIMETZ/infinities), HANA `catalog`/`errors`, and Mongo `query` limits; unit/native tests and clippy pass.

# sqlr-app: application logic
No new bug found in this cycle. Reviewed pending-change routing, closed-tab reopening, store-worker duplication and the welcome/recent paths. The full-suite flake (`history_context_menu_saves_the_query_as_a_snippet`) is a 5 s wall-clock timeout that reproduces under heavy parallel load but passed 5/5 in isolation; it is test timing, not a product race.

# sqlr-ui: grid and editor
No new bug found in this cycle. Reviewed selection/subtract, edit history/duplicate/set-NULL, document-table pages, editor statements/wrapping and hex parsing; 167 unit + targeted integration tests and clippy pass.

# sqlr-ui: forms, trees, dialogs and keymap
No new bug found in this cycle. Reviewed plan reveal, lock trees, snippet folder ids, XML pretty-printing, JSON view/edit validation and the keymap editor; 166 unit + targeted integration tests and clippy pass.
