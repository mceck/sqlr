# DuckDB

A sidecar driver (`drivers/sqlr-driver-duckdb`) that embeds the bundled DuckDB engine through
the official `duckdb` Rust crate. Every sqlr release publishes it for each platform: choose
**Download and install** in the Driver Manager (Settings → Drivers…), like SAP HANA. To install a
build of your own instead:

```sh
scripts/package-driver.sh duckdb duckdb.tar.gz    # builds it in release mode and packs it
```

then **Install from file…** in the Driver Manager. The driver needs no Java, server or system
library. It is not part of the app because the engine alone is about 37 MB, which would take the
app (about 50 MB) past its 75 MB budget.

## Connection

Choose **DuckDB** in **New connection** and select a database file, or enter `:memory:` for a
temporary database. The read-only option opens an existing file without allowing changes.
Connection URLs include `duckdb:///absolute/path/data.duckdb`, `duckdb:relative.duckdb` and
`duckdb::memory:`.

The sidecar process serves every connection of the driver. Sessions of one connection (editor
tabs, the explorer) share one native database, each with its own transaction; an in-memory
database lives as long as its connection.

## Driver behavior

The driver supports SQL queries, bound parameters, transactions, cancellation, chunked results,
schema/table/view browsing, table descriptions and autocomplete catalogs. Decimal and integer
values that require more precision than sqlr's numeric storage are kept as text. Time-zone
offsets and supported timestamp fractions are preserved.

DuckDB extensions are separate from the embedded engine. Optional extensions such as remote
storage integrations may require an explicit `INSTALL` and `LOAD` and are subject to DuckDB's
extension policies; the driver disables extension autoloading and installs nothing at startup.
Existing `jdbc-duckdb` connections continue to use their installed JDBC driver.

`TIME_NS` result columns currently require an explicit `CAST(column AS VARCHAR)` because the
upstream Rust row decoder cannot read that native type safely. The driver reports an
unsupported-type error before executing such a statement.

## Testing

`cargo nextest run -p sqlr-driver-duckdb` runs the conformance suite and the native tests
against the real embedded engine, and drives the sidecar executable over the protocol. No
server is needed.
