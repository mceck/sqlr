# sqlr

sqlr is a lightweight, native database client for relational, document and key-value databases. This glossary fixes the words used for its concepts in code, specs and tasks.

## Connections

**Connection**:
A saved set of settings that identifies one database server or file and how to reach it, without its secrets.
_Avoid_: datasource, server, connection config (for the saved form)

**Folder**:
A named group of Connections in the explorer.

**Connection URL**:
A URL such as `postgresql://user@host:5432/db` that fills a Connection's form.

**Connection profile**:
A named policy a Connection follows: its commit mode, read-only flag, confirmations and color. The built-in ones are Development, Test and Production.
_Avoid_: production flag, environment

**Commit mode**:
How statements on a Session are committed: Auto, Manual or Smart.

**Credential vault**:
The encrypted file that holds every secret (passwords, SSH passphrases, the MCP token), whose key lives in the OS keychain.
_Avoid_: secret store, keychain (for the vault itself)

**Known database**:
An entry of the catalog of databases sqlr knows about, marked bundled, compatible or planned.

**Demo database**:
The sample SQLite database sqlr generates on first use for the welcome screen.

## Drivers

**Driver**:
The component that speaks one database's protocol and exposes it to sqlr through the driver interface.

**Family**:
The shape of a Driver's data and operations: Relational, Document or KeyValue.

**Capability**:
A flag a Driver declares for an optional feature (transactions, server cancel, editable results, explain, …); the UI offers an action only when the Driver declares it.

**Dialect**:
The per-database rules a relational Driver provides: quoting, statement splitting, keywords, DML and DDL generation, statement classification.

**Driver manifest**:
The description a Driver publishes of itself: id, name, Family, Capabilities, Dialect and connection-form fields.

**Bundled driver**:
A Driver compiled into the app.

**Sidecar driver**:
A Driver that runs as a separate process and talks to the app over the sidecar protocol.
_Avoid_: plugin, external driver

**Driver archive**:
A packaged Sidecar driver for one platform, downloaded from the sqlr release or picked from disk.
_Avoid_: local driver file, driver package

**Driver Manager**:
The window where the user installs, updates, rolls back and removes Sidecar drivers.

**JDBC bridge**:
The Sidecar driver that connects any database with a JDBC driver through a Java runtime.

**Driver release**:
The GitHub release of the running app's version, from which Sidecar drivers are downloaded.

**Active version**:
The installed version of a Driver that new Sessions start on.

**Conformance suite**:
The shared test harness every Driver passes against a real database.

## Engine

**Session**:
One live connection to a database opened by the engine for a Connection, owning its transaction state, commit mode and read-only switch.
_Avoid_: connection (for the live link), engine session

**Editor session**:
The Session that belongs to one Editor tab when per-editor sessions are on.

**Server session**:
A session as the database server sees it, listed and terminated from the administration panels.

**Operation**:
Any in-flight piece of client work (connect, catalog read, query, fetch, script, Job), listed in the Operations panel and cancellable on its own.

**Job**:
A background Operation with progress and an outcome: applying edits, export, import, DDL applies, transfers, dumps.
_Avoid_: task (for a running job)

**Result**:
The rows or documents one statement returns, held by the engine and fetched in pages.

**Result chunk**:
A columnar block of rows in sqlr's binary format; a **Doc chunk** is its counterpart for documents.

**Result grid**:
The virtualized table widget that shows a Result's rows.

**Page size**:
How many rows a Result fetches at a time; 500 by default.
_Avoid_: fetch size

**Catalog**:
The metadata of a Connection's databases, schemas and objects, used by the explorer, autocomplete and the palette.

**Catalog cache**:
The stored copy of the Catalog that lets the explorer and autocomplete work before the server answers.

**Node**:
One entry of the explorer tree, identified by its **Node path** from the Connection down.

## Editing

**Changeset**:
The pending edits to a Result's data (modified cells, inserted rows, deleted rows) until they are applied or discarded.
_Avoid_: pending changes, edit buffer

**Row key**:
The primary key or the `NOT NULL` unique key that identifies a row for editing.

**Schema change list**:
The pending edits in an Object editor until their DDL is applied.
_Avoid_: changeset (for schema edits)

**Statement class**:
How the Dialect classifies a statement: read, write, DDL, destructive, transaction control or other.

**Destructive statement**:
A statement that loses data in bulk, such as `DROP`, `TRUNCATE`, `ALTER … DROP`, or `DELETE` and `UPDATE` without `WHERE`.

## Workspace

**Explorer**:
The left-hand tree of Folders, Connections and their Nodes.
_Avoid_: schema tree, navigator

**Editor tab**:
A tab holding a query editor bound to one Connection and, optionally, one database.
_Avoid_: console, query tab, script tab

**Object editor**:
A tab that shows and alters one database object through sub-tabs and a Schema change list.

**DDL viewer**:
A read-only Editor tab showing an object's DDL.

**Value panel**:
The side panel that shows and edits the selected cell, document or key value in full.

**Key browser**:
The Redis view that lists keys grouped by prefix with their types.

**Query language**:
The lexing and statement-splitting rule an Editor tab uses: SQL, the Mongo shell, the Elasticsearch console, Redis commands or JSON.

**Statement scope**:
The relations, aliases and common-table-expression names of the statement around the caret, used by autocomplete.

**Palette**:
The command palette: open object (⌘P) and run command (⌘⇧P).

**Operations panel**:
The panel that lists every Operation with its state and a Cancel action.

## Tools

**Data transfer**:
Moving rows between two Endpoints through sqlr's own engine; import and export are its presets.

**Endpoint**:
A source or target of a Data transfer: a table, a query, a file, a workbook sheet or the test-data generator.

**Dump** and **Restore**:
Backing up and restoring a database with its native command-line tools.

**Schema compare**:
The diff of two schemas or databases with a generated migration script.

**ER diagram**:
The entity-relationship drawing of a schema, laid out from its foreign keys.

**Explain plan**:
A statement's execution plan, shown in the Plan pane as a tree of operators.

**Test-data generator**:
The tool that fills tables with generated rows through a Data transfer.

## Library

**History**:
The searchable record of every executed statement with its Connection, duration and outcome.
_Avoid_: query log

**Copy format**:
The text shape a copy produces: TSV, CSV, JSON, SQL `INSERT` or Markdown.

**Typed clipboard**:
The cell values kept inside the app next to their copied text, so a paste restores types.
_Avoid_: clipboard buffer

**Snippet**:
A named, saved query, global or scoped to one Connection.

## Settings

**Setting level**:
Where a setting value is set: global, Connection profile or Connection, each overriding the one before.

**Safety flag**:
A setting that a lower Setting level or an agent can switch on but never off.
_Avoid_: tighten-only setting (outside code)

**Keymap**:
The mapping of commands to keyboard chords.

**Theme**:
The curated light or dark color set the window, panels and grid draw with.

**Toast**:
The short confirmation or error notice that appears over the window and fades.

## Agents

**MCP server**:
The local server through which AI agents use sqlr's tools while the app runs.

**Agent**:
An AI client connected to the MCP server.

**Agent access**:
The level a Connection grants to Agents: None, Read or Write.

**Agent Session**:
The Session the engine opens for one MCP session on one Connection, isolated from the explorer and editor Sessions.

**MCP token**:
The persistent bearer token every MCP request must carry, stored in the Credential vault.

**Discovery file**:
The token-free `mcp.json` that publishes the running MCP server's URL and process metadata.

**MCP session**:
One Agent's connection to the MCP server; its Sessions and open transactions end with it.

**Approval**:
The in-app dialog where the user allows or denies an Agent's destructive or configuration-deleting request.

**Integrated terminal**:
The shell terminal docked in the app, available to the user only.

**Terminal workspace**:
The persistent folder the Integrated terminal starts in, holding the agent guidance and the sqlr skills.
_Avoid_: terminal folder

**Detected agent**:
A supported agent client whose configuration or installation sqlr finds on this machine.

**Agent installation**:
sqlr's MCP server entry in a Detected agent's user-level configuration, written by Install and kept current by sqlr.
_Avoid_: MCP config (for an installed entry)

**sqlr skill**:
One of the `sqlr-*` agent skills that teach an Agent a sqlr workflow, kept in the Terminal workspace and downloadable on its own.

## Diagnostics

**Incident**:
A record of an unexpected failure, written as a local file.

**Diagnostics bundle**:
The bounded report the user previews, copies, saves or hands to a GitHub issue.

**Log ring**:
The bounded in-memory copy of the most recent log lines.

**Startup notice**:
The next-launch banner that describes a panic or an unclean shutdown.

**App session**:
One run of the app from launch to exit, identified in logs and Incidents.
_Avoid_: session (for an app run)
