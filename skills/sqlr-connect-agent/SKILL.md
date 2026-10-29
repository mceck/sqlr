---
name: sqlr-connect-agent
description: Connect this agent to sqlr, the desktop database client, and fix a missing or failing sqlr MCP connection. Use when the sqlr tools are not available, when a sqlr tool call fails with connection refused, 401, not_exposed, secret_required or an empty connection list, or when the user asks how to use sqlr from an agent.
metadata:
  sqlr-version: 0.1.4
---

# Connect to sqlr

sqlr is a desktop database client. While it runs, it can host a local MCP server that agents use to browse the user's databases, run queries and work in the user's editor tabs. The server lives only as long as the app: no daemon, no remote endpoint.

The tools are reachable when three things are true:

1. sqlr is open and its MCP server is on (Settings → Agents).
2. sqlr installed its server entry into this agent's configuration (Settings → Agents → Install), and the agent was restarted afterwards.
3. The user exposed at least one Connection to agents (Agent access `Read` or `Write`).

## Diagnose in this order

**The sqlr tools are not in your tool list.** This agent is not installed. Ask the user to open sqlr, go to Settings → Agents, press **Install** next to this agent, then restart the agent. If this agent is not in the list, the user can use **Other agent… → Copy MCP config** and add the configuration in the agent's own format. The copied configuration holds a secret token: the user pastes it; you never ask for it.

**Calls fail with connection refused.** sqlr is closed, or its MCP server is off. Ask the user to open sqlr and turn on the server in Settings → Agents (or with the toolbar toggle).

**Calls fail with 401 Unauthorized.** The token was rotated or the port changed. sqlr rewrites the configuration of every agent it installed, so restarting the agent is usually enough. If Settings → Agents shows this agent as **Outdated**, ask the user to press **Update**, then restart.

**`config_list` returns no connections, or a call answers `not_exposed`.** Agents see only the Connections the user exposed. Ask the user to set Agent access to `Read` or `Write` on the Connection (connection dialog → Advanced, the Explorer context menu, or Settings → Agents). Agents cannot change Agent access.

**A call answers `secret_required`.** The Connection has no stored password or key for the named fields. Ask the user for the values and write them with `config_write`, or ask the user to connect once in sqlr. See the `sqlr-create-connection` skill.

**A call answers `read_only`, `denied` or `timeout`.** These are decisions, not faults: the Connection is exposed read-only, the profile forbids writes, or the user denied (or did not answer) an Approval. Report it to the user and do not retry the same statement in a loop.

## Is sqlr running?

sqlr publishes a token-free discovery file while its server runs. Read it to learn the URL, port, process id, protocol and app version:

| Platform | Discovery file |
| --- | --- |
| macOS | `~/Library/Application Support/dev.sqlr.sqlr/mcp.json` |
| Linux | `$XDG_DATA_HOME/sqlr/mcp.json` (usually `~/.local/share/sqlr/mcp.json`) |
| Windows | `%APPDATA%\sqlr\sqlr\data\mcp.json` |

No file means the server is not running. Read it only; it never contains the token.

## Rules

- Never edit this agent's MCP configuration by hand to add or fix sqlr: Install and Update in sqlr own that entry.
- Never ask for, print, log or copy the MCP token, and never look for sqlr's credential vault.
- Never try to work around exposure, read-only mode or Approvals. They are the user's consent.
- The integrated terminal inside sqlr is a normal shell for the user; no MCP tool reaches it.
