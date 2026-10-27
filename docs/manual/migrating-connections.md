[Manual](README.md) › Connections

# Move saved connections between clients

Import connection definitions to avoid entering the same server settings again. Export them to back up your setup or use it in another client. This moves connection settings, rather than database contents.

## Import connections

1. Choose **File → Import…**, or **Import connections…** on the welcome screen.
2. Select a detected source, choose **Choose file…**, drop a file onto the dialog, or choose **Paste from clipboard**.
3. Review the recognised connections and migration notes.
4. Select the connections to keep, then choose the import button, labelled with the selected count.
5. Choose **Done** when saving finishes.

No database connection is opened during import. sqlr skips duplicate endpoints, database names, users and SSH settings, including duplicates across source clients. New connections retain supported folders and receive unique names. Agent access starts at **None**.

| Source | What you can read |
| --- | --- |
| sqlr | Connection backup JSON, versions 1 and 2 |
| DBeaver | A project's `.dbeaver/data-sources.json` |
| DataGrip / JetBrains | `dataSources.xml`, including copied data-source XML; sibling local settings when available |
| TablePlus | XML or binary `Connections.plist`, or connection URLs |
| pgAdmin | Exported servers JSON |
| Beekeeper Studio | Connection JSON or the local `app.db` database |
| Tabularis | Plaintext export JSON or `connections.json` |

Files and clipboard input are limited to 16 MiB. Automatic discovery checks standard client locations. You can select another file when discovery does not find your project.

## Review credentials and settings

Imports keep supported portable fields, including applicable TLS and SSH options. They do not bring across arbitrary client preferences, startup commands, custom JDBC properties or custom profile definitions. Recognised production and staging environments map to the built-in **Production** and **Test** profiles.

Credentials explicitly included in supported sources are saved in sqlr's credential vault, and credential saving is enabled for those connections. DBeaver and DataGrip passwords remain in their own stores. Beekeeper's encrypted database columns are not read. Connections without imported credentials ask when you connect.

Read every migration note before connecting. Local certificate and key paths may need changing. Encrypted TablePlus archives are unsupported. Re-export encrypted Tabularis backups as plaintext; use a full export for linked SSH profiles. Unsupported transports and policies are reported rather than silently removed.

## Export a backup or another client's format

1. Choose **File → Export…**.
2. Select the destination in **Export to**.
3. Select the connections. Definitions that the format cannot represent are unavailable.
4. If needed and supported, enable **Include credentials**.
5. Choose the export button and a destination.
6. Follow the destination instructions in the completion screen.

> **Warning:** **Include credentials** writes plaintext secrets. Keep the resulting file private. The option starts off each time and resets when you change formats.

| Destination | Output and how to use it |
| --- | --- |
| sqlr | JSON backup; restore with **File → Import…** |
| DBeaver | Additional `data-sources-sqlr.json`; put it in the project's `.dbeaver` folder without replacing `data-sources.json`, then restart DBeaver |
| DataGrip / JetBrains | XML; copy its contents and use the destination's clipboard import |
| TablePlus | One URL per line; import each URL |
| pgAdmin | PostgreSQL servers JSON; use its server import |
| Beekeeper Studio | A new folder containing one JSON file per connection |
| Tabularis | One JSON export with groups and linked SSH profiles |

Credential export is supported for sqlr, TablePlus, Beekeeper Studio and Tabularis. A sqlr backup can include every saved secret field; TablePlus URLs include the database password. The other two include supported database and SSH credentials.

DataGrip and TablePlus exports exclude SSH connections. Beekeeper and Tabularis export PostgreSQL, MySQL/MariaDB and SQLite, excluding SSH jump hosts; Tabularis also excludes SSH agent authentication. Choose sqlr's format for connections other formats cannot preserve.

## Related

- [Connections](connections.md): verify imported access and certificates.
- [Move database data](moving-data.md): transfer rows instead of connection settings.

---

← [Connections](connections.md) · [Explore and change objects](explorer-and-objects.md) →
