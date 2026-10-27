[Manual](README.md) › Reference

# Understand sqlr's terms

Use this glossary when a label in sqlr or a procedure in the manual is unfamiliar.

## Connections and access

| Term | Meaning |
|---|---|
| Connection | Saved settings for reaching a database server or file. Passwords are stored separately |
| Folder | A group of saved connections in the explorer |
| Connection URL | An address you can paste to fill a connection form |
| Connection profile | A named set of commit rules, protections and color, such as Development or Production |
| Session | A live database connection, with its own transaction state |
| Editor session | The session used by one editor tab when separate sessions are enabled |
| Server session | A database server's view of an active connection, shown in administration |
| Credential vault | The encrypted file holding saved passwords, SSH secrets and the MCP token; the OS keychain protects its key |
| Demo database | The sample SQLite database available from the welcome screen |

## Browse, query and edit

| Term | Meaning |
|---|---|
| Explorer | The tree of connections, databases, schemas and objects on the left |
| Catalog | Information about database objects, used by browsing, autocomplete and object search |
| Catalog cache | A saved copy of that information, which may need refreshing after server changes |
| Editor tab | A query editor associated with a connection and, when applicable, a database |
| Palette | Keyboard search for objects or commands |
| Result grid | The table displaying returned rows |
| Page size | The number of rows requested with each fetch |
| Value panel | The panel showing the selected cell, document or key value in full |
| Changeset | Your unapplied cell edits, inserted rows and deleted rows |
| Row key | The primary key or non-null unique key identifying a row for editing |
| Object editor | A tab for inspecting and changing an object's structure |
| Schema change list | Structural edits waiting for you to apply their generated DDL |
| DDL | SQL that defines or changes database objects, such as `CREATE TABLE` |
| DDL viewer | A read-only tab showing an object's definition |
| Commit mode | Auto, Manual or Smart rules for starting and committing transactions |
| Destructive statement | A statement that can remove data in bulk, such as `DROP`, `TRUNCATE` or a `DELETE` without `WHERE` |
| Key browser | The Redis view of keys grouped by prefix and value type |
| Copy format | The text representation of copied cells: for example TSV, JSON or SQL `INSERT` |
| Typed clipboard | Values sqlr keeps alongside copied text to preserve their types when pasted within the app |
| History | The searchable record of executed statements and their outcomes |
| Snippet | A saved query, available globally or for one connection |

## Tools and drivers

| Term | Meaning |
|---|---|
| Operation | A piece of work currently running or recently finished in sqlr |
| Job | A background operation with progress, such as importing rows or applying edits |
| Operations panel | The list where you inspect progress and cancel work |
| Data transfer | Moving data between a source and a destination, including import and export |
| Endpoint | A transfer's source or destination, such as a query, table or file |
| Dump / Restore | Backing up or restoring a database with its native tools |
| Schema compare | A comparison of structures with a generated migration script |
| ER diagram | A drawing of tables and their foreign-key relationships |
| Explain plan | A database's description of how it will execute a statement |
| Driver | The component that lets sqlr communicate with a database |
| Capability | An optional feature a driver supports; unavailable features are hidden or disabled |
| Bundled driver | A driver included in the application |
| Sidecar driver | A driver running in a separate process |
| Driver archive | An installable sidecar driver package for your platform |
| Active version | The installed driver version used by new sessions |
| JDBC bridge | A sidecar driver that uses Java and a JDBC driver to reach a database |

## Settings, agents and reports

| Term | Meaning |
|---|---|
| Setting level | Global, connection profile or connection scope for a preference |
| Safety flag | A protection that a more specific setting can enable but cannot weaken |
| Keymap | The commands and keyboard shortcuts selected by your preset and customizations |
| MCP server | sqlr's local interface for agent tools, available while the application runs |
| Agent access | A connection's **None**, **Read** or **Write** permission for agents |
| Agent session | An agent's separate database session, isolated from your editors |
| MCP token | The credential an MCP client must send to use the server |
| Discovery file | The token-free `mcp.json` describing the running MCP endpoint |
| Approval | The dialog where you allow or deny an agent's protected request |
| Incident | A local record of an unexpected application failure |
| Diagnostics bundle | A bounded report you can review, copy, save or share |
| App session | One run of sqlr, from launch to exit |
| Startup notice | A banner reporting an earlier panic or unclean shutdown |

## Related

- [Getting started](getting-started.md): see these concepts in your first workflow.
- [Settings and shortcuts](settings-and-shortcuts.md): configure the workspace.

---

← [Agents and terminal](agents.md) · [Troubleshooting](troubleshooting.md) →
