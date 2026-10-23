# Agents and the terminal

sqlr can expose a local MCP server while it runs, so Claude Code, Codex or another MCP
client can see the connections and tabs you already have open. It can also open an
integrated terminal at the bottom of the window, which is where you normally start the
agent.

The MCP server is **off by default**. The terminal works with MCP off: it is a normal
terminal first.

## What an agent can and cannot do

| Can | Cannot |
|---|---|
| Browse and search schemas, describe tables | See hosts, ports, users, TLS/SSH options or driver values |
| Run one statement or native command per call, with result paging | Read a secret, even one it wrote |
| Read and edit an open SQL query tab with a revision check | Create, close or save a tab |
| Run a tab's script, one statement, or the selection | Connect or disconnect a connection |
| Read, create, update, move and delete snippets (delete asks first) | Install or remove drivers |
| Read profiles, settings, keymap, tasks and driver fields | Change MCP settings, app updates or `terminal.shell` |
| Write connection values, secrets and configuration | Loosen a safety flag, reset a tightened setting or move to a looser profile |
| See the current state of long operations and cancel them | Read or drive the integrated terminal |

Every connection starts at **None**: an agent cannot see it. You set **Read** or **Write**
in the connection dialog (Main tab, below the profile and colour), or in the explorer's
**Agent access** menu. The explorer shows a small eye next to exposed connections.

- **Read** forces the agent's database session read-only. Browsing and reads work;
  writes are rejected before they run.
- **Write** allows writes, but the connection profile still applies: read-only,
  confirmation of writes, confirmation of destructive statements and commit mode.

An agent-created connection is **None**, and duplicating a connection resets its exposure
to **None**. The agent cannot change exposure.

## Enable MCP and connect a client

1. Open **Settings → Agents**.
2. Turn on **Enable agent access (MCP)**. The server starts on `127.0.0.1`, port `47821`
   by default, and the status shows the endpoint and active session count.
3. Press **Copy MCP config** and paste it into your client's MCP configuration. It
   includes the bearer token; treat it like a password.
4. Open the terminal with `Ctrl+Backtick` (the same on macOS, Windows and Linux), or use
   **Copy MCP config** in the terminal header.

The token is generated once and lives in the same encrypted credential vault as connection passwords. The OS keychain protects the vault's encryption key, and the application shares one unlock per session. It is not written to
`connections.toml`, the database, logs or the discovery file. **Rotate token** replaces it
and disconnects every active agent immediately. The endpoint and discovery file exist
only while sqlr runs; they are removed on disable or quit.

## Approvals

When the profile asks for confirmation, an agent's statement opens the same modal
approval as a statement you run yourself. It shows the client name and version, MCP
session, connection, profile, database, exposure, risk label and the exact statement.
**Deny** has focus; `Escape` denies; the request is denied automatically after 60 seconds.
Configuration deletes and snippet deletes always ask. Secret writes never ask.

If an approval is denied or times out, the agent receives a structured `denied` or
`timeout` error.

## Limits and troubleshooting

- Results are capped at 200 rows and 256 KiB by default (up to 1000 rows), and remain
  pageable by id for five minutes after the last access.
- At most eight MCP sessions; an idle one is closed after 30 minutes.
- **Port busy:** another program holds the port. sqlr keeps running without MCP; choose
  another port in Settings → Agents (the server restarts).
- **Keychain error:** sqlr will not fall back to a plaintext token. Unlock the keychain
  and enable the server again.
- **401 Unauthorized:** the client has the wrong or an old token. Copy the configuration
  again or rotate the token.
- **403 Forbidden:** the request did not come from the loopback `Host`/`Origin`; remote
  and browser origins are rejected by design.
- **Timed-out approval / expired result:** retry the tool call. An expired result must be
  re-run.

The rotating application log records tool calls (client, session, tool, target, outcome,
duration and numeric result metadata) but never SQL text, native commands, rows, secrets,
connection values, the shell command or terminal output. There is **no persistent audit
table**: the log is the record and token rotation is the containment control. Query
History still shows the queries that ran.

## The integrated terminal

The terminal is a real PTY with your login shell. It starts lazily the first time you open
it, keeps running when hidden, and is resized at the bottom of the window above the status
bar. The toolbar button, **View → Terminal**, the command palette and the keymap all
toggle it.

- ANSI 16-colour, 256-colour and truecolour; bold, dim, italic, underline, reverse video;
  cursor styles; wide and combining characters; alternate screen; scrollback; resize.
- Selection, copy and bracketed paste. Copy is `Cmd+C` on macOS and `Ctrl+Shift+C`
  elsewhere; paste is `Cmd+V` / `Ctrl+Shift+V`; `Ctrl+C` is always the interrupt.
- **Restart**, **Clear**, **Kill** and the exit code are in the header. **Kill** terminates
  the process group; quitting sqlr kills it too.
- Shell command, working directory and scrollback live in **Settings → Terminal** and
  apply on Restart. The shell command is never shown to agents and never logged; do not
  embed credentials in it.
- No MCP token is put in the terminal environment and no agent is started automatically.
  Mouse reporting and clickable links are not implemented.

The terminal is not exposed to MCP: no tool can read it, write to it, start it or stop it.
An agent you launch inside it is an ordinary MCP client and follows the same exposure,
approval and logging rules.
