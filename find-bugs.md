# Bug hunt

## Sections:
[x] sqlr-core: values, encoding and documents (`value`, `encode`, `read`, `rows`, `chunk`, `doc*`, `formats`)
[] sqlr-core: SQL and text utilities (`classify`, `syntax`, `format_sql`, `generate`, `schema_change`, `compare`, `datagen`, `transfer`, `url`, `wire`, `es_request`, `mongo_shell`, `er`)
[] sqlr-store: persistence and secrets (connections, vault, settings, history, snippets, keymap, recents, profiles, layered)
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
[to fill]

# sqlr-store: persistence and secrets
[to fill]

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
