# sqlr manual

*Last updated: 27 October 2026.*

sqlr is a desktop database client for people who query databases, maintain their structure and move data between systems. You can use relational databases, MongoDB, Elasticsearch and Redis from the same window. Saved connections keep the details you need to return to your work; drivers determine which tools each database offers.

This manual describes the application as shipped. Database permissions still apply to every action. An action that your driver does not support is hidden or disabled.

## Where to start

| Your work | Start with |
|---|---|
| First time using sqlr | [Get your first result](getting-started.md), using the local demo database |
| Querying and editing data | [Write and run SQL](sql-editor.md), then [Work with results](results-and-editing.md) |
| Maintaining a database | [Profiles and transactions](profiles-and-transactions.md), then [Administration](administration.md) |
| Moving data | [Import, export and backup](moving-data.md) |
| Connecting an AI client | [Agents and the terminal](agents.md), including connection access levels |

## Connect and explore

- [Getting started](getting-started.md): install sqlr, open the demo database and run your first query.
- [Connections](connections.md): save connection settings, configure SSH and TLS, and reconnect.
- [Import and export connections](migrating-connections.md): bring saved connections from another client or export them for another setup.
- [Explorer and object editors](explorer-and-objects.md): find objects, inspect DDL and apply structure changes.

## Query and change data

- [SQL editor](sql-editor.md): run statements and scripts, use completion, and reuse history and snippets.
- [Results and data editing](results-and-editing.md): fetch, sort, copy, inspect and change rows.
- [Profiles, transactions and safety](profiles-and-transactions.md): choose commit behavior and control writes.

## Move and maintain

- [Import, export, transfer and backup](moving-data.md): map columns, run transfers and use native backup tools.
- [Schema tools](schema-tools.md): compare structures, draw relationships, read query plans and generate test data.
- [Administration](administration.md): manage roles, inspect sessions and locks, and monitor operations.
- [Document and key-value databases](nosql.md): query documents and work with Redis keys.

## Configure and get help

- [Drivers](drivers.md): install sidecar drivers and manage their versions and prerequisites.
- [Settings and shortcuts](settings-and-shortcuts.md): change preferences, key bindings and storage locations.
- [Agents and the terminal](agents.md): grant MCP access, handle approvals and use the integrated shell.
- [Glossary](glossary.md): understand the terms used throughout the manual.
- [Troubleshooting](troubleshooting.md): resolve common problems and prepare a local diagnostics report.

## Find your way around the window

The **Explorer** on the left contains your connections and database objects. The center holds editor and object tabs. Query results appear below their editor; the value panel lets you inspect a selected value in full. The toolbar shows the active connection and execution controls. The status bar reports the active session, transaction state and result timing.

Use ⌘P to find an object and ⇧⌘P to find a command. Keyboard examples use macOS notation; use Ctrl in place of ⌘ on Windows and Linux unless a page gives a platform-specific chord. You can change bindings in **Preferences → Keymap**.

Work that takes time appears in **Operations**. You can cancel an operation while continuing to work in another tab. Grid edits and schema edits wait for **Apply**; statements you run in the editor follow the session's commit mode instead.

---

[Start with your first query](getting-started.md) →
