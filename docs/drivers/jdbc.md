# JDBC bridge

Any database with a JDBC driver — SQL Server, Oracle, Db2, ClickHouse, Snowflake, H2, DuckDB, or
anything else — through one sidecar: a small Java 17 program (`drivers/jdbc-bridge`, built with
`javac` and `jar`, no build tool) started by a Rust launcher (`crates/sqlr-driver-jdbc`).

## Requirements

Java 17 or newer: `JAVA_HOME`, the system Java on macOS, or `java` on `PATH`. Without one, the
driver says how to install it (in the Driver Manager when you install, in the connection dialog
when you connect).

## Drivers

| Driver id | Database | Form |
|---|---|---|
| `jdbc` | Generic JDBC | JDBC URL, driver class, driver jar, more jars, user, password |
| `jdbc-sqlserver` | SQL Server | host, port, database, user, password |
| `jdbc-oracle` | Oracle | host, port, service name, user, password |
| `jdbc-db2` | Db2 | host, port, database, user, password |
| `jdbc-clickhouse` | ClickHouse | host, port, database, user, password |
| `jdbc-snowflake` | Snowflake | host, database, user, password |
| `jdbc-duckdb` | DuckDB | file |
| `jdbc-h2` | H2 | file or `mem:` URL part, user, password |

The registry entry of each carries the Maven coordinates (group, artifact, version, sha256) of its
JDBC jar; the Driver Manager downloads them into the driver's `jars/` directory and refuses a jar
whose sha256 differs from the signed index. Generic JDBC takes any jar you have: pick it in the
form (or list more, separated by `;`).

## Install from a file

```sh
scripts/package-driver.sh jdbc-h2 jdbc-h2.tar.gz --jar ~/Downloads/h2-2.3.232.jar
```

then **Install from file…** in the Driver Manager. `scripts/package-driver.sh jdbc …` makes the
generic bridge.

## Types

Values arrive as the right column kind or as text, never as a driver object's `toString()`:
`DECIMAL` beyond a double is exact text (decimal hint), `TIMESTAMP WITH TIME ZONE` keeps its offset
(ISO text), `BLOB` is bytes, `CLOB` text (cut at 16 MiB, with the cut shown), `ARRAY` is
`[1, 2, NULL]`-style text, `UUID`/`JSON`/`INTERVAL` use the driver's text, and an object the bridge
cannot show is `‹unsupported TYPE›`.

## Limits of this release

- Statements run in auto-commit mode (no transactions), and there is one connection per session.
- Cancel uses `Statement.cancel()`; a driver that ignores it gets its process killed after 5 s.
- Browsing and describing use `DatabaseMetaData`; a database that reports little there shows little.
- Only the H2 integration tests run here; the other presets are untested against real servers.
- NoSQL over JDBC is out of scope.
