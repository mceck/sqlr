# ClickHouse

The bundled `clickhouse` driver uses the [official pure Rust client](https://github.com/ClickHouse/clickhouse-rs). It connects directly over HTTP or HTTPS with rustls, without Java or JDBC jars.

Choose **ClickHouse** on the welcome screen or in **New connection**:

| Field | Default |
|---|---|
| Host | `localhost` |
| Port | `8123` |
| Database | `default` |
| User | `default` |
| Password | Empty; stored in the credential vault |
| TLS | Off; enable for HTTPS and select the server's HTTPS port, usually `8443` |

Connection URLs use `clickhouse://user@host:8123/database`. For HTTPS, set `tls=true` in the URL query or enable TLS in the form. Enter the password separately. Certificate verification uses the operating system's trusted certificates.

SQL results stream in pages with column names and types, including empty results. Large unsigned integers, wider integers and decimals remain exact text. Dates, UUIDs and arrays carry their display hints. The explorer lists databases, tables, views, materialized views and columns, and the catalogs supply completion.

Cancel stops the active server query. Closing an unfinished result releases it and cancels its query. Catalog browsing and other statements remain available between result pages.

Statements run in auto-commit. Use the connection form for the default database and a query's `SETTINGS` clause for query settings; `USE`, general `SET`, temporary session state and explicit transactions are unavailable. The read-only switch applies the server's readonly setting to each request. Grids are read-only because ClickHouse sorting keys do not uniquely identify rows. Inserts and other writes remain available from the SQL editor; imports requiring transactional loading are unavailable.

Saved `jdbc-clickhouse` connections continue using their installed JDBC bridge. Create a new ClickHouse connection to use the Rust driver.
