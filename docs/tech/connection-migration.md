# Connection import and export

File → Import/Export and the welcome screen's **Import connections…** link open
connection migration. Files are recognized automatically; the import preview lets
users select connections and skips duplicates. Credential-store I/O runs on the
store worker.

**Include credentials** is off each time export opens and resets when the format
changes. When enabled, exports contain plaintext secrets and the dialog warns
about this. Only saved credentials from selected connections are read. sqlr
backups with credentials use version 2; metadata-only backups keep version 1.
Both versions import into sqlr, with credentials stored separately in its vault.
Export files use owner-only permissions on Unix. Agent consent is never imported. Neither passwords nor SSH secrets are written
to `connections.toml`.

| Format | Import source | Export | Credentials |
|---|---|---|---|
| sqlr | Backup JSON v1/v2 | Single JSON | All saved secret fields |
| TablePlus | Plist or connection URLs | URLs, one per line | Database password in URL on export |
| DBeaver | data-sources.json | Additional data-sources-sqlr.json | Separate credential store; not included |
| DataGrip | dataSources.xml or clipboard XML | XML | Separate credential store; not included |
| pgAdmin | Servers JSON | Servers JSON | Format does not support passwords |
| Beekeeper Studio | Connection JSON or local app.db | New folder with one native JSON file per connection | Database and SSH passwords/key passphrase in JSON; encrypted app.db credentials are skipped |
| Tabularis | Plaintext export JSON or connections.json | Single native export JSON with nested groups and linked SSH profiles | Database and SSH passwords/key passphrase |

Beekeeper and Tabularis exports currently support PostgreSQL, MySQL/MariaDB and
SQLite. Other drivers and SSH jump hosts are excluded with a migration note; Tabularis
also excludes SSH agent authentication. Beekeeper preserves agent authentication
and excludes incoming bastion profiles.
Beekeeper folder assignment is chosen in its import UI; TLS modes that do not map
exactly produce a review note. Tabularis imports use the primary database from
multi-database profiles, report this limitation, and skip unsupported Kubernetes
and AWS SSM transports. Encrypted Tabularis exports must first be exported as
plaintext from Tabularis. Local Tabularis profiles referencing an external SSH
profile require a full export containing that profile.

Automatic discovery checks standard macOS, Linux and Windows application data
locations for Tabularis connections.json and Beekeeper app.db, in addition to the
existing clients. Beekeeper databases are opened read-only, restricted to the
personal workspace, and their encrypted password columns are never read.

Schema references:

- [Beekeeper JSON importer](https://github.com/beekeeper-studio/beekeeper-studio/blob/master/apps/studio/src/backend/lib/objectimport/connection.ts)
- [Beekeeper saved connection fields](https://github.com/beekeeper-studio/beekeeper-studio/blob/master/apps/studio/src/common/appdb/models/saved_connection.ts)
- [Tabularis export and connection models](https://github.com/TabularisDB/tabularis/blob/main/src-tauri/src/models.rs)
- [Tabularis connection export documentation](https://tabularis.dev/wiki/connections)
- [pgAdmin import/export restrictions](https://www.pgadmin.org/docs/pgadmin4/latest/import_export_servers.html)
- [DBeaver credential storage](https://dbeaver.com/docs/dbeaver/Admin-Manage-Connections/)
- [DataGrip data source sharing](https://www.jetbrains.com/help/datagrip/managing-data-sources.html)
