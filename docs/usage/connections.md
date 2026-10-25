# Connections

A connection tells sqlr how to reach a database: where it is, who you are, and how to talk to it safely. Saved connections appear in the explorer. You can put them in folders and drag them into the order you like.

## Databases you can connect to

| Database | Notes |
|---|---|
| PostgreSQL | TLS, SSH tunnel, an option to list every database on the server |
| MySQL and MariaDB | TLS, SSH tunnel |
| SQL Server | SQL authentication, TLS, SSH tunnel |
| Oracle | Works directly; the Oracle Instant Client is not needed |
| SQLite | A file on disk, optionally opened read-only |
| DuckDB | Installed from a driver file; a file on disk or `:memory:`, optionally opened read-only. No Java needed |
| Snowflake | Built in; HTTPS SQL API with OAuth, PAT or JWT token authentication |
| MongoDB | TLS, SSH tunnel |
| Elasticsearch | Versions 7.17 and 8.x |
| Redis | A standalone server, with TLS and ACL users |
| SAP HANA | Installed from a driver file, see [Drivers](drivers.md) |
| Db2, ClickHouse, H2 and any other JDBC database | Through the JDBC bridge, installed from a driver file. Needs Java 17 or newer |

Several databases that speak the PostgreSQL or MySQL protocol are listed in the new-connection dialog with their usual port and defaults already filled in: CockroachDB, YugabyteDB, Amazon Redshift, Greenplum, TimescaleDB, Neon, Supabase, Amazon Aurora (both flavors), TiDB and SingleStore.

## Creating a connection

Open **New connection** and pick the database. The form comes from the driver, so each database asks for the fields that make sense for it. For PostgreSQL that is host, port, database, user, password and the SSL settings. For SQLite it is a file path with a file picker.

Some things worth knowing:

- **Paste a URL.** Paste something like `postgresql://alice@db.example.com:5432/shop` into the URL field and the form fills itself. `mysql://...`, `sqlite:/path/to/file.db`, `duckdb:/path/to/file.duckdb` and `snowflake://account.snowflakecomputing.com/database` work the same way.
- **Passwords stay out of URLs.** If a URL contains a password, sqlr asks you to remove it and type the password in the Password field. This keeps passwords out of your clipboard history and your logs.
- **Save password.** Turn it on to keep the password in sqlr's encrypted credential vault, protected by your operating system's keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service). sqlr unlocks it on first use and shares that unlock across connections until you quit. Turn it off and sqlr asks every time you connect.
- **Test.** Tries the connection and reports the server version and the round-trip time, so you know the details are right before you rely on them.
- **Profile.** Pick Development, Test or Production. The profile sets the connection's color and how careful sqlr is with writes. See [Profiles, transactions and safety](profiles-and-transactions.md).

## DuckDB and Snowflake

DuckDB comes from a driver file (see [Drivers](drivers.md)) that embeds the whole engine, so nothing else is needed. Pick a database file, or use `:memory:` for a temporary database. Snowflake uses its public SQL API. Enter the account hostname, optional database, schema, warehouse and role, then choose the token type and paste the token in its secret field. Tokens use the same encrypted vault as passwords.

Snowflake requests run independently in auto-commit mode. Persistent transactions, temporary tables, session variables and `USE` changes across requests are unavailable; select the context in the connection form. Editing result cells is unavailable. Existing JDBC connections still work with their installed driver. More in [the Snowflake notes](https://github.com/mceck/sqlr/blob/main/docs/drivers/snowflake.md).

## SSH tunnels

Turn on the SSH tab to reach a database through a bastion host. You provide:

- the SSH host, port and user
- the authentication: a password, or a private key (with its passphrase if it has one)
- optionally a **jump host** and its user, for setups with one hop in front of the bastion

sqlr opens the tunnel, connects through it and closes it when you disconnect. Dump and restore tools use the same tunnel, so a backup from a private database works too.

## TLS

For databases that support it, the connection form has the TLS fields. For PostgreSQL the SSL mode ranges from `disable` to `verify-full`, and you can provide a root certificate, a client certificate and a client key.

Snowflake always uses HTTPS with certificate verification. Other network connections start unencrypted unless the driver configuration turns TLS on, so for any server outside your own machine, choose a mode that suits it. sqlr supports TLS 1.2 and newer. Servers that only offer TLS 1.0 or 1.1 cannot be reached.

## Working with a connection

Right-click a connection in the explorer for these actions: connect and disconnect, edit, copy the connection URL, refresh, and remove. The connection chip in the toolbar does the same for the connection of the active tab, and also lets you switch to another one.

If a connection fails, the explorer shows the error under it with a **Retry** button. If a connection drops while you work, the tab shows a banner with **Reconnect**.

## One session per tab

By default each query tab gets its own database session, and the explorer uses another. That means a long query in one tab never makes another tab or the explorer wait. Transactions, read-only mode and commit mode belong to the tab's session.

If a server limits the number of connections you can open, turn off **A session per editor tab** in Settings, under Connections. All tabs then share one session per connection.

## Where your connections are stored

Connections are saved in `connections.toml` in the config directory, without passwords. Because the file holds no secrets, you can share it with a teammate or keep it in version control. Passwords stay in the encrypted `secrets.vault` on each machine; its encryption key stays in that machine's keychain. The key is tied to the config directory, so copying or moving the vault alone does not transfer credentials. The location of every file sqlr uses is listed in [Settings and shortcuts](settings-and-shortcuts.md#where-things-are-stored).

Passwords saved by older versions migrate automatically when used. Your OS may still ask permission for each old keychain item during migration. sqlr saves the encrypted copy before removing the old item; afterward, connections share a single vault unlock per application session. A keychain denial or damaged vault is reported without replacing your saved credentials.
