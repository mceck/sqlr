# Bug hunt

## Sections:
[x] sqlr-core: values, encoding and documents (`value`, `encode`, `read`, `rows`, `chunk`, `doc*`, `formats`)
[x] sqlr-core: SQL and text utilities (`classify`, `syntax`, `format_sql`, `generate`, `schema_change`, `compare`, `datagen`, `transfer`, `url`, `wire`, `es_request`, `mongo_shell`, `er`)
[x] sqlr-store: persistence and secrets (connections, vault, settings, history, snippets, keymap, recents, profiles, layered)
[] sqlr-engine: sessions, transactions and results (`engine`, `session`, `tx`, `script`, `apply`, `result`, `result_memory`, `result_spill`, `job`)
[] sqlr-engine: transfer and imports (`transfer/*`)
[] sqlr-sidecar: protocol, client and process
[] sqlr-registry: driver install, index and verification
[] sqlr-mcp: server, policy, tokens and results
[] sqlr-terminal: PTY, screen, input and tabs
[] drivers: PostgreSQL
[] drivers: MySQL / MariaDB
[] drivers: SQLite
[] drivers: Redis
[] drivers: SQL Server / Oracle
[] drivers: Elasticsearch / Snowflake / ClickHouse / JDBC
[] sidecar drivers: MongoDB / DuckDB / HANA
[] sqlr-app: application logic (workers, actions, messages, cli, tabs, pending)
[] sqlr-ui: grid and editor
[] sqlr-ui: forms, trees, dialogs and keymap

## Findings:
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
[to fill]

# sqlr-engine: transfer and imports
[to fill]

# sqlr-sidecar: protocol, client and process
[to fill]

# sqlr-registry: driver install, index and verification
[to fill]

# sqlr-mcp: server, policy, tokens and results
[to fill]

# sqlr-terminal: PTY, screen, input and tabs
[to fill]

# drivers: PostgreSQL
[to fill]

# drivers: MySQL / MariaDB
[to fill]

# drivers: SQLite
[to fill]

# drivers: Redis
[to fill]

# drivers: SQL Server / Oracle
[to fill]

# drivers: Elasticsearch / Snowflake / ClickHouse / JDBC
[to fill]

# sidecar drivers: MongoDB / DuckDB / HANA
[to fill]

# sqlr-app: application logic
[to fill]

# sqlr-ui: grid and editor
[to fill]

# sqlr-ui: forms, trees, dialogs and keymap
[to fill]
