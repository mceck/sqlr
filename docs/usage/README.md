# Using sqlr

sqlr is a database client. You connect to a database, look around, run queries, edit data and move it from one place to another. This guide explains how each of those things works, in the order you will probably meet them.

Keyboard shortcuts are written for macOS (⌘ is Command). On Windows and Linux, ⌘ becomes Ctrl.

## Start here

| If you want to... | Read |
|---|---|
| Install sqlr and run your first query | [Getting started](getting-started.md) |
| Connect to a database, through SSH or TLS | [Connections](connections.md) |
| Move saved connections from DBeaver, DataGrip, TablePlus or pgAdmin | [Import and export connections](migrating-connections.md) |
| Browse tables and change their structure | [The explorer and object editors](explorer-and-objects.md) |
| Write and run SQL comfortably | [The SQL editor](sql-editor.md) |
| Read, sort, filter and edit results | [Results and data editing](results-and-editing.md) |
| Control how and when changes are committed | [Profiles, transactions and safety](profiles-and-transactions.md) |
| Move data in and out, or back it up | [Import, export, transfer and backup](moving-data.md) |
| Compare schemas, draw diagrams, read query plans | [Schema tools](schema-tools.md) |
| Manage users, sessions and server settings | [Administration](administration.md) |
| Work with MongoDB, Elasticsearch or Redis | [Document and key-value databases](nosql.md) |
| Add support for another database | [Drivers](drivers.md) |
| Change the theme, formats or shortcuts | [Settings and shortcuts](settings-and-shortcuts.md) |
| Use an agent with sqlr, or the integrated terminal | [Agents and the terminal](agents.md) |

## The window at a glance

- **Left:** the explorer. Every saved connection lives here, in folders you can rearrange by drag and drop. Expanding a connection shows its databases, schemas, tables and so on.
- **Center, top:** tabs. A tab is a query editor, a table, an object editor, a collection or a Redis database.
- **Center, bottom:** results for the query you just ran, plus Messages, History and the Operations list.
- **Right:** the value panel, which shows the full content of the selected cell or document.
- **Toolbar:** the current connection, the Run and Stop button, commit mode and Run script.
- **Status bar:** connection state, commit mode, transaction state, cursor position, row count and timing.

Panels are docked, so you can drag them where you want them. The arrangement is remembered.

## Two ideas that help everywhere

**Everything that can take time runs in the background.** Connecting, loading a schema, running a query and exporting a table never freeze the window. The Operations list shows what is running and lets you cancel any of it, including a connection attempt that is still waiting for a server.

**Changes wait for your approval.** Edits in the grid and in object editors collect as a pending list. You see the SQL that will run, then you press Apply, or Discard to throw the changes away.
