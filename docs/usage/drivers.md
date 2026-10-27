# Drivers

A driver is what lets sqlr talk to a particular database. PostgreSQL, MySQL and MariaDB, SQL Server, Oracle, SQLite, Snowflake, MongoDB, Elasticsearch and Redis come with the app, ready to use. SAP HANA, DuckDB and the JDBC bridge are separate **driver files**: sqlr downloads them for you from its release on GitHub, or installs one you have on disk.

## Downloading a driver

1. Open the **Driver Manager** (from Settings, under Drivers, or the palette).
2. Select the driver under **Available** and choose **Download and install**.

sqlr downloads the driver built for your computer from the GitHub release of the sqlr version you are running, checks it against the release's SHA-256 checksums and starts it once before keeping it. Nothing is downloaded until you ask: sqlr does not check for driver updates in the background. After you update sqlr, the Driver Manager offers **Update to** the new version for the drivers you installed.

The download also starts from the new-connection dialog: choosing a database whose driver is missing opens the Driver Manager on that driver.

## Installing a driver from a file

If your computer cannot reach GitHub, or you built a driver yourself:

1. Open the **Driver Manager** and choose **Install from file...**, then pick the driver archive. The release's files (`sqlr-driver-<id>_<version>_<platform>.tar.gz`) work here too.
2. Read the review screen. It shows the driver's id, version, size and SHA-256 checksum, and reminds you that a driver runs with your own rights and receives the passwords of the connections that use it.
3. Confirm. Nothing is installed or started before you do.

A connection from another machine whose driver is not one sqlr publishes opens the file picker directly.

Drivers go in your data directory under `drivers/<id>/<version>/`. Install only files you got from a source you trust, since sqlr shows you what is in the file and leaves the decision about trusting it to you.

## Managing versions

The Driver Manager lists every installed driver with all its versions. You can:

- search the list
- install a newer version next to the old one
- **roll back** to an older version, which takes effect without restarting sqlr
- remove one version, or a driver entirely
- restart a driver

Because several versions stay on disk side by side, trying a new driver version is low risk: if it misbehaves, switch back.

## How drivers run

A driver that does not ship with the app runs as its own process. sqlr starts it when the first connection needs it and stops it after it has been idle for a while (the idle time is set in Settings, under Drivers). If a driver crashes, only the connections that use it fail. You see an error and a **Restart driver** action, and the rest of sqlr carries on.

## SAP HANA

Download it in the Driver Manager (or package it yourself with `scripts/package-driver.sh hana hana.tar.gz` and install the archive). The connection form asks for host, port, tenant database, user, password and default schema, and offers TLS with a custom CA. Behind Docker, NAT or an SSH tunnel, **Pin the redirect to this host** makes sqlr connect straight to the tenant's SQL port instead of following the address HANA advertises. Details are in [the HANA notes](https://github.com/mceck/sqlr/blob/main/docs/drivers/hana.md).

## DuckDB

The DuckDB engine is large, so it ships as a driver file instead of inside the app: download it in the Driver Manager (or package it with `scripts/package-driver.sh duckdb duckdb.tar.gz` and install the archive). The driver embeds DuckDB itself: no Java, server or system library is needed. The connection form asks for a database file (or `:memory:`) and whether to open it read-only. Details are in [the DuckDB notes](https://github.com/mceck/sqlr/blob/main/docs/drivers/duckdb.md).

## JDBC databases

Any database that has a JDBC driver can be reached through the JDBC bridge. It needs Java 17 or newer on your machine, found through `JAVA_HOME`, the system Java on macOS or `java` on your `PATH`.

Presets exist for Db2, ClickHouse and H2, with Snowflake and DuckDB presets retained for existing connections, and a generic one takes any JDBC URL, driver class and jar. The Driver Manager downloads the bridge for Generic JDBC, Db2, ClickHouse and H2; the JDBC jar of your database is not part of it, so pick the jar in the connection form, or package it with the driver yourself:

```sh
scripts/package-driver.sh jdbc-h2 jdbc-h2.tar.gz --jar ~/Downloads/h2-2.3.232.jar
```

Statements through the bridge run in auto-commit mode. More in [the JDBC notes](https://github.com/mceck/sqlr/blob/main/docs/drivers/jdbc.md).

## Writing your own

If your database is not on the list, you can write a driver in Rust with `sqlr-driver-sdk`, or in any language that can speak the sidecar protocol. Start with [Writing a driver](https://github.com/mceck/sqlr/blob/main/docs/specs/drivers.md).
