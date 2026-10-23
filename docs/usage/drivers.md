# Drivers

A driver is what lets sqlr talk to a particular database. PostgreSQL, MySQL and MariaDB, SQL Server, Oracle, SQLite, MongoDB, Elasticsearch and Redis come with the app, ready to use. Every other database comes from a **driver file** that you install yourself.

## Installing a driver from a file

1. Open the **Driver Manager** (from Settings, under Drivers, or the palette).
2. Choose **Install from file...** and pick the driver archive.
3. Read the review screen. It shows the driver's id, version, size and SHA-256 checksum, and reminds you that a driver runs with your own rights and receives the passwords of the connections that use it.
4. Confirm. Nothing is installed or started before you do.

The same flow starts by itself when you create a connection for a database whose driver is missing: the Driver Manager opens and points you at **Install from file...**.

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

A driver installed from a file runs as its own process. sqlr starts it when the first connection needs it and stops it after it has been idle for a while (the idle time is set in Settings, under Drivers). If a driver crashes, only the connections that use it fail. You see an error and a **Restart driver** action, and the rest of sqlr carries on.

## SAP HANA

Package the driver with `scripts/package-driver.sh hana hana.tar.gz`, then install the archive. The connection form asks for host, port, tenant database, user, password and default schema, and offers TLS with a custom CA. Behind Docker, NAT or an SSH tunnel, **Pin the redirect to this host** makes sqlr connect straight to the tenant's SQL port instead of following the address HANA advertises. Details are in [the HANA notes](../drivers/hana.md).

## JDBC databases

Any database that has a JDBC driver can be reached through the JDBC bridge. It needs Java 17 or newer on your machine, found through `JAVA_HOME`, the system Java on macOS or `java` on your `PATH`.

Presets exist for Db2, ClickHouse, Snowflake, DuckDB and H2, and a generic one takes any JDBC URL, driver class and jar. The JDBC jar for your database is packaged with the driver or picked in the connection form:

```sh
scripts/package-driver.sh jdbc-h2 jdbc-h2.tar.gz --jar ~/Downloads/h2-2.3.232.jar
```

Statements through the bridge run in auto-commit mode. More in [the JDBC notes](../drivers/jdbc.md).

## Writing your own

If your database is not on the list, you can write a driver in Rust with `sqlr-driver-sdk`, or in any language that can speak the sidecar protocol. Start with [Writing a driver](../writing-a-driver.md).
