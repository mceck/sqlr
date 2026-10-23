# Move connections between SQL clients

Open **File → Import…** to bring saved connections into sqlr. The dialog searches the
standard DBeaver, DataGrip and TablePlus locations in the background and selects new
connections automatically. Review the preview and click **Import**. Nothing connects to
a database during this operation.

You can also drop a connection file onto the dialog, choose **Choose file…**, or use
**Paste from clipboard**. File and clipboard formats are recognized automatically.
For DataGrip, use **Copy Data Source** and paste its XML directly into sqlr.

Existing connections are skipped by endpoint, database and user, including duplicates
from several detected clients. Name collisions receive a numeric suffix. Imports save
all selected connections together and leave existing definitions and credentials intact.

## Supported sources

| Client | Source |
|---|---|
| DBeaver | `<workspace>/<project>/.dbeaver/data-sources.json` |
| DataGrip / JetBrains | `.idea/dataSources.xml` or Copy Data Source XML; the sibling `dataSources.local.xml` is read when available |
| TablePlus | `Connections.plist` (binary or XML), JSON connection arrays, or a text file with connection URLs |
| pgAdmin | `servers.json` from Tools → Import/Export Servers |
| sqlr | A connection backup created by File → Export… |

The preview includes folders, supported TLS and SSH settings, and recognized environment
profiles where the source provides them. Missing drivers can be installed when connecting.
Read the migration notes for skipped connections and settings that require attention.
Client-specific preferences, startup scripts and custom profile definitions are not migrated.

Passwords, tokens and private key contents are never copied. Enter credentials when
connecting in the destination client. Local certificate and private key paths may need
adjustment on another machine. sqlr agent access is always off for imported connections.
Unsupported network handlers, read-only policies and ambiguous TablePlus TLS modes are
reported and skipped rather than silently discarded. Encrypted `.tableplusconnection`
archives are not supported; choose TablePlus's local connection list or connection URLs.
Passwords embedded in URLs are discarded automatically.

## Export

Open **File → Export…**, choose the destination client, select connections and click
**Export**. Connections that the selected format cannot represent are unavailable.
The completion screen explains how to load the file in the destination client.

| Destination | Output and next step |
|---|---|
| sqlr | JSON backup of portable connection settings; use File → Import… to restore |
| DBeaver | Additional `data-sources-sqlr.json`; place in the project's `.dbeaver` directory and restart DBeaver |
| DataGrip / JetBrains | XML; copy its contents and use New → Import from Clipboard |
| TablePlus | One connection URL per line; use Import from URL for each connection |
| pgAdmin | PostgreSQL server JSON; use Tools → Import/Export Servers |

DataGrip and TablePlus URL exports currently support PostgreSQL, MySQL, SQLite and
SQL Server connections without SSH. DBeaver also supports Oracle; pgAdmin supports
PostgreSQL. Choose sqlr for other drivers or to retain the portable SSH settings.

Format references: [DBeaver connection files](https://dbeaver.com/docs/dbeaver/Data-Sources-Json-Reference/),
[JetBrains data source sharing](https://www.jetbrains.com/help/datagrip/managing-data-sources.html),
[TablePlus connections](https://docs.tableplus.com/gui-tools/manage-connections),
[pgAdmin server definitions](https://www.pgadmin.org/docs/pgadmin4/latest/import_export_servers.html).
