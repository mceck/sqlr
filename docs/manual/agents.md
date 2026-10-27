[Manual](README.md) › Integrations

# Use an agent or the integrated terminal

Connect a local MCP client to work with sqlr's database tools and open editor tabs. You can also use the integrated terminal as a shell, with agent access turned on or off.

## Decide which connections an agent may use

MCP is off by default. Turning it on does not expose your connections: each connection has a separate **Agent access** level.

| Level | Agent access |
|---|---|
| **None** | The connection is hidden and cannot be queried |
| **Read** | Browse objects and run reads in a read-only agent session |
| **Write** | Run reads and writes, subject to the profile's read-only setting and confirmations |

1. Open **Settings…** and select **Agents**.
2. Find the connection under **Connection access**, using **Search connections** or the **All**, **Exposed** and **Hidden** filters.
3. Select **Read** or **Write**. The change applies and saves immediately.

You can also change **Agent access** in the explorer context menu or the connection dialog's **Advanced** section. New, imported, duplicated and agent-created connections start at **None**. An agent cannot change this permission. Setting it back to **None** closes that connection's agent sessions.

## Connect your MCP client

1. In **Agents**, enable **Enable agent access (MCP)**.
2. Wait for the running status. The default endpoint is `http://127.0.0.1:47821/mcp`.
3. Choose **Copy MCP config** and paste the copied configuration into your client's MCP settings.
4. Connect the client while sqlr remains open.

The configuration includes a bearer token. Keep it private. **Copy endpoint** copies only the address. The token-free `mcp.json` discovery file in the data directory also publishes the endpoint while the server runs.

**MCP port** accepts ports 1024 through 65535. Changing it restarts the server and disconnects clients. **Rotate token** replaces the credential and disconnects all MCP sessions. Copy the configuration again afterward. Disabling MCP or quitting sqlr ends agent work and removes the discovery file.

## Understand what tools can do

| Area | Available to an agent | Kept under your control |
|---|---|---|
| Database | Browse, describe, search and query exposed connections | Exposure level and profile protections |
| Editor | Read and edit existing query text with a revision check; run a statement, selection or script | Creating, closing and saving editor tabs |
| Library | Read and manage snippets and saved configuration | Approval for snippet and configuration deletion |
| Configuration | Read permitted settings; update permitted connection settings and secrets | MCP settings, application updates and terminal shell command |
| Operations | Inspect progress and cancel work | Integrated terminal input, output and lifecycle |
| Credentials | Write a credential when needed | Reading stored secrets |
| Drivers | Inspect declared driver fields | Installing and removing drivers |

An agent session has its own database transaction, separate from your editor and other clients. Ending an MCP session rolls back its open transactions. An agent cannot loosen a safety flag or move a connection to a less protective profile.

When a tool reports `secret_required`, provide the missing connection credentials through sqlr or your approved configuration workflow, then retry.

## Review an approval

When a profile requires confirmation, an agent's request opens an approval showing the client, connection, profile, risk and exact statement. Read the statement before allowing it.

**Deny** is initially focused. Escape denies, and an unanswered approval expires after 60 seconds. Deleting configuration or a snippet also requires approval. An agent receives `denied` or `timeout` when the request is refused or expires.

The server supports eight MCP sessions. Idle sessions close after 30 minutes. Result pages default to 200 rows, accept up to 1,000 rows and stay within 256 KiB. A result expires after five minutes without access; rerun it after expiry.

## Open a terminal

1. Choose **View → Terminal**, use the toolbar toggle, or press Ctrl+Backtick.
2. Use your shell. It starts when the terminal first opens and keeps running while the panel is hidden.
3. Use the header actions to **Clear** the display, **Restart** the shell or **Kill** its process group.

Quitting sqlr also stops the terminal process group. A shell that has exited shows its exit code.

| Action | macOS | Windows and Linux |
|---|---|---|
| Copy selected text | ⌘C | Ctrl+Shift+C |
| Paste | ⌘V | Ctrl+Shift+V |
| Interrupt command | Ctrl+C | Ctrl+C |

In **Settings… → Terminal**, an empty **Shell command** uses your login shell. An empty **Working directory** uses sqlr's configuration `terminal` folder, containing agent instructions and MCP configuration. **Font size** applies immediately; restart the shell for the remaining terminal changes.

Terminal output and the configured shell command are excluded from logging. MCP cannot read or control this terminal. sqlr does not start an agent automatically or put the MCP token in the shell environment. An agent you launch yourself uses the same connection access rules as any other MCP client.

## Fix a connection problem

| Message or symptom | What to do |
|---|---|
| **Port busy** | Choose another **MCP port**, or stop the program using it |
| **Keychain error** | Unlock the OS keychain, then disable and enable MCP again |
| `401 Unauthorized` | Copy the current MCP configuration; a token may have been rotated |
| `403 Forbidden` | Use the local loopback endpoint with a local client; remote hosts and unrelated browser origins are rejected |
| Connection absent or `not_exposed` | Set that connection's **Agent access** to **Read** or **Write** |
| Expired result or approval | Rerun the query or request and review the new approval |

## Related

- [Connections](connections.md): save credentials and configure database access.
- [Profiles and transactions](profiles-and-transactions.md): understand write protections.
- [Troubleshooting](troubleshooting.md): review local logs and reports.

---

← [Settings and shortcuts](settings-and-shortcuts.md) · [Glossary](glossary.md) →
