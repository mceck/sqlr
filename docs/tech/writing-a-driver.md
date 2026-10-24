# Writing a driver

An sqlr driver for a database sqlr does not ship with is a small program that implements
the Rust `Driver` traits and serves them to sqlr over the sidecar protocol
(`docs/driver-protocol.md`). It can be written in Rust with `sqlr-driver-sdk`, or in any
language that can read and write the protocol's frames.

## From the template

```sh
cargo generate --path templates/sqlr-driver-template --name my-driver
cd my-driver
cargo test
```

The skeleton answers `SELECT 1`. Fill in, in `src/lib.rs`:

1. **The manifest**: the driver id (saved connections name it, so choose it once), a
   display name, the connection form (`FieldSpec`s: text, port, password, file, choice, …)
   and the capabilities you support (`transactions`, `server_cancel`, `schemas`, …). Fields
   marked `secret` are kept in the encrypted credential vault (with an OS-keychain-protected key) and arrive only in `connect`.
2. **`Driver::connect`**: open your database connection from `ConnectionConfig` and
   `Secrets`.
3. **`Connection::ping`, `browse`, `cancel_handle`**: server info, the explorer tree
   (`Node`s with a `NodePath` and a `NodeKind`) and a handle that stops the running
   statement.
4. **`RelationalConn::execute`**: run SQL and return either `Execution::Affected(n)` or a
   `RowStream`. Build chunks with `ChunkBuilder`; each column has a physical `ColumnKind`
   (bool, int, float, text, bytes, mixed) and a `DisplayHint` (decimal, temporal, json, …).
   Exact numbers and dates travel as text with the right hint, never as lossy floats.
5. **`RelationalConn::dialect`**: quoting, placeholders, keywords. sqlr sends this to the
   editor for highlighting, completion and generated SQL.

Optional interfaces (`describe`, `catalog`, `execute_with`, `introspect`, `admin`) switch on
features such as editing results and the object editors; leave them out until you need
them.

### Automatic values in table descriptions

For result editing, `RelationalConn::describe` returns `TableColumn` metadata. Set
`generated` when the column refuses explicit values (computed columns and identities
that always generate). Set `automatic` when the server supplies the value if omitted,
including identities, sequence-backed defaults and rowid aliases. An automatic column
may still allow explicit values. The grid uses this distinction when duplicating rows
and when regenerating IDs for pasted rows. Sidecar descriptions that omit `automatic`
remain valid and default it to `false`.

## Rules

- **stdout is the protocol.** Never `println!`; use `eprintln!` or `tracing` to stderr.
- **Never log secrets.** `Secrets`' `Debug` prints keys only; keep it that way.
- Return `DriverError` with the right `ErrorKind` (`Syntax` with a `position` puts a marker
  in the editor, `Cancelled` ends a cancelled statement quietly).
- Honour cancel: when `cancel_handle().cancel()` returns, the running statement must stop
  within a few seconds, or sqlr kills your process.
- Lengths and counts you read from the database are untrusted too: never size an
  allocation from them without a bound.

## Test it with the conformance suite

```rust
let target = sqlr_driver_tests::Target::new("mydb", Box::new(|| Box::pin(async { /* connect */ })))
    .with_setup([
        "DROP TABLE IF EXISTS sqlr_conformance",
        "CREATE TABLE sqlr_conformance (n INTEGER PRIMARY KEY, t TEXT)",
        /* … 25 rows: n = 1..=25, t = 'row <n>' */
    ]);
let report = sqlr_driver_tests::relational_suite(&target).await;
assert!(report.failed.is_empty(), "{report}");
```

Set `target.capabilities.server_cancel`, `target.slow_sql` (a statement that runs long
unless cancelled) and `target.type_cases` (queries returning one value, with the value
they must return) to switch on the cancel and type-matrix checks. Run it in-process first,
then through `SidecarDriver` to check the protocol layer.

## Install

`cargo build --release`, package it with `scripts/package-driver.sh`, then in sqlr:
Settings → Drivers… → *Install from file…*. A driver file is the only install source; sqlr
shows its id, version and SHA-256 and asks for explicit confirmation before running it.
