# Agents: MCP access and integrated terminal

## Problem Statement

People who use sqlr together with coding agents have to bridge the two worlds by hand.
The agent cannot see the connections, schemas, open queries or snippets that sqlr has
already loaded; the person copies SQL and schema snippets back and forth, pastes query
results into the agent, and manually applies the agent's corrections to the editor. The
same happens with configuration: renaming a connection, adding an SSH option or saving a
snippet means leaving the agent, editing the form and returning.

There is also nowhere to run the agent without leaving sqlr. A separate terminal window
loses the connection between the agent and the running application, and the person has to
configure the MCP endpoint in yet another place.

The feature has to solve this without turning the app into a remote-control surface for
any process on the machine. Agents act with the user's rights, can read data and can write
configuration; the design must make exposure explicit, keep credentials out of reach,
preserve the existing transaction and confirmation rules, and leave a clear audit trail in
the app log.

## Solution

sqlr exposes a **native MCP server** while the application is running. It listens only on
the loopback interface, is disabled by default and is enabled and configured in
**Settings → Agents**. A persistent bearer token lives in the shared encrypted credential vault, protected by the OS keychain, and a
permission-protected discovery file publishes the endpoint to local tools. The server
speaks MCP Streamable HTTP with a small, typed tool surface: a guide, catalog browsing,
queries, editor and snippet operations, and write-only configuration management. Each
connection has an **agent access** level (`None`, `Read`, `Write`) chosen by the person;
`None` is the default.

Agents can inspect schemas, run SQL and native document/key-value commands on exposed
connections, read and page results, edit one of the open SQL query tabs with revision
checks, run that tab in context, and manage snippets and configuration. Configuration
follows a strict split: connection values and secrets are **write-only**; profiles,
settings, keymap and task definitions are readable. Destructive statements and
configuration deletions go through the same in-app approval the person already sees, with
a timeout. Driver installs, the MCP server settings, application updates and the terminal
shell command remain human-only. Secrets can be written but never read.

sqlr also gains an **integrated terminal** at the bottom of the workspace. It is a real
PTY with a login shell, toggled with `Ctrl+Backtick` on every platform, and it keeps
running when hidden. The person can launch Claude Code, Codex or any other agent in it.
The terminal is not exposed to MCP and its output is never logged; a **Copy MCP config**
action in its header makes configuring the agent a single paste. The terminal uses the
app theme and monospace font and behaves like a normal terminal: ANSI colours, alternate
screen, wide characters, resize, selection, copy/paste and scrollback.

The result is one application where the person, the database and the agent share the
same context, with exposure, credentials and destructive operations governed by explicit
rules instead of ad-hoc copy/paste.

## User Stories

### MCP setup, transport and security

1. As a developer, I want sqlr to expose an MCP server only while the app is open, so that no background daemon keeps running after I quit.
2. As a security-conscious user, I want MCP to be disabled by default, so that no local endpoint exists until I explicitly enable it.
3. As a user, I want a stable loopback URL and a persistent bearer token, so that I configure my agent once instead of after every restart.
4. As a user, I want the token stored in the shared encrypted credential vault with its key protected by the OS keychain and rotatable from Settings, so that I can revoke access without editing files or authorizing a separate token keychain item each session.
5. As a user, I want the endpoint, port, status and active session count in Settings, so that I can diagnose why an agent cannot connect.
6. As a user, I want a discovery file with the URL, process id and protocol version but no token, so that local tooling can find the endpoint without exposing the secret.
7. As a user, I want the server to bind only to `127.0.0.1` and reject non-loopback `Origin` and `Host` headers, so that websites and other machines cannot reach it.
8. As a user, I want a clear error when the configured port is already in use, so that a second sqlr instance does not crash or silently take over.
9. As a user, I want the server to stop and remove its discovery file when I disable it or quit, so that stale endpoints do not linger.
10. As a user, I want a session cap and an idle timeout, so that a forgotten client cannot hold resources forever.
11. As a user, I want to copy a complete MCP client configuration from Settings, so that connecting an agent is one paste.
12. As a user, I want to see the connected client name and version in approvals and logs, so that I know which agent is acting.
13. As a user, I want to rotate the token and drop active sessions, so that a leaked token can be revoked immediately.
14. As a user, I want MCP tools only, without resources or prompts, so that the surface stays predictable and uniformly protected.
15. As an agent, I want a guide tool that explains the application and the other tools, so that I can onboard myself without guessing.

### Exposure and data access

16. As a user, I want every connection to default to not exposed, so that no database is reachable by agents without my consent.
17. As a user, I want to choose `None`, `Read` or `Write` per connection, so that I can allow inspection without allowing modifications.
18. As a user, I want the exposure control in the connection dialog, so that it is set where the connection itself is defined.
19. As a user, I want exposure to be human-only, so that an agent cannot expose a connection it created.
20. As a user, I want a duplicated connection to reset to not exposed, so that copies do not silently inherit agent access.
21. As an agent, I want to see only the metadata of exposed connections, so that I can choose a target without reading infrastructure values.
22. As an agent, I want to browse schemas, search objects and describe tables, so that I can write correct queries.
23. As an agent, I want read exposure to force read-only execution, so that a mistake cannot modify data.
24. As an agent, I want write exposure to respect the connection profile, so that production confirmations still apply.
25. As a user, I want the agent's connections to open lazily and visibly, so that I do not manage connect or disconnect on its behalf.
26. As a user, I want an agent not to disconnect my connections, so that its work cannot interrupt mine.
27. As a user, I want an agent's sessions isolated from my editor's transactions, so that its `BEGIN` and `COMMIT` cannot affect my work.
28. As a user, I want the agent's open transactions rolled back when its MCP session ends, so that nothing is left uncommitted.
29. As an agent, I want a `secret_required` error when credentials are missing, so that I can ask for or write them.
30. As an agent, I want to query SQL, MongoDB, Elasticsearch and Redis through one family-aware tool, so that I do not need different APIs.
31. As a user, I want read exposure enforced for every family, so that MongoDB, Elasticsearch and Redis writes are also blocked.
32. As a user, I want agent queries in Operations and History, so that I can see what ran and watch long operations.

### Approvals and safety

33. As a user, I want destructive agent statements to open the same confirmation dialog as my own statements, so that no agent has a bypass.
34. As a user, I want the approval to show client, connection, database and the exact statement, so that I can decide safely.
35. As a user, I want approvals to time out and deny, so that a stuck agent does not wait forever.
36. As a user, I want configuration deletes and snippet deletes to require approval, so that an agent cannot silently destroy my work.
37. As a user, I want safety flags in profiles and settings to be tighten-only for agents, so that an agent cannot disable its own guardrails.
38. As a user, I want driver installs, MCP server settings, application updates and the terminal shell command to be human-only, so that an agent cannot gain code execution or open a remote endpoint.
39. As a user, I want secret writes to be autonomous but logged by field name only, so that connection setup is fast without exposing values.
40. As a user, I want secrets never returned by any tool, so that credentials never enter the model context.
41. As a user, I want connection values to be write-only, so that hosts, users, options and other infrastructure details are not disclosed.
42. As a user, I want profiles, settings, keymap and task definitions readable, so that agents can avoid breaking policy.
43. As a user, I want a structured `denied` or `timeout` error when I refuse or miss an approval, so that the agent can explain what happened.

### Tools, results, editor and snippets

44. As an agent, I want a synchronous query result with a row and byte cap, so that one call is useful without flooding my context.
45. As an agent, I want to page a result by id, so that I can inspect more without rerunning the query.
46. As an agent, I want to cancel a query, so that a long scan does not block me.
47. As an agent, I want results to expire, so that forgotten results do not exhaust the application's memory.
48. As an agent, I want typed columns and arrays of rows, so that duplicate column names are unambiguous.
49. As an agent, I want exact decimals, timestamps and UUIDs as strings, so that precision is preserved.
50. As an agent, I want binary values in base64 and JSON columns parsed, so that I can handle every value class.
51. As an agent, I want to list the open SQL query tabs, so that I can choose one to fix.
52. As an agent, I want to read a tab's text and revision, so that I can propose a correction.
53. As an agent, I want to replace the whole text or apply precise edits with a revision check, so that I do not overwrite what the person is typing.
54. As a user, I want agent edits to be one undo step, visibly highlighted and marked dirty, so that I can review and revert them.
55. As an agent, I want to run the tab's whole buffer as a script, a single statement or the current selection, so that I can validate the fix in context.
56. As a user, I want a busy tab to reject an agent's run, so that my running query is not silently cancelled.
57. As an agent, I want to read, create, update, move and delete snippets, so that I can save reusable SQL.
58. As a user, I want snippet deletion approved, so that an agent cannot lose my library.
59. As an agent, I want to create and update connections, folders, profiles, settings, keymap and tasks, so that I can automate setup.
60. As an agent, I want to delete connections, folders, profiles and tasks only with approval, so that destructive configuration changes are explicit.
61. As an agent, I want driver manifests, so that I can create connections with the right fields.
62. As an agent, I want to know whether a secret exists without reading its value, so that I can complete a connection.
63. As an agent, I want settings changes applied to the running application, so that they take effect immediately.
64. As a user, I want a profile that is in use not to be deleted, so that connections do not break.

### Integrated terminal

65. As a user, I want an integrated terminal at the bottom that opens with a toggle, so that I can run an agent without leaving sqlr.
66. As a user, I want the terminal toggle next to the value-panel and history toggles, so that all workspace panels are in one place.
67. As a user, I want `Ctrl+Backtick` on every platform, so that the terminal is one keystroke away.
68. As a user, I want a real PTY with a login shell, so that Claude Code, Codex and my tools on `PATH` work normally.
69. As a user, I want the shell command, working directory and scrollback configurable, so that the terminal fits my environment.
70. As a user, I want the terminal to keep running when hidden, so that a long agent task is not interrupted.
71. As a user, I want Kill, Restart and the exit code, so that I can recover from a dead or wedged shell.
72. As a user, I want ANSI colours, cursor, alternate screen, wide characters and resize, so that TUI agents render correctly.
73. As a user, I want selection, copy, paste and bracketed paste, so that I can work efficiently and paste multi-line prompts safely.
74. As a user, I want no mouse reporting or clickable links in the first version, so that the terminal stays simple and predictable.
75. As a user, I want the terminal to use my theme and monospace font, so that it feels native to sqlr.
76. As a user, I want a Copy MCP config action in the terminal header, so that I can configure the agent from where I launch it.
77. As a user, I want no automatic token environment variables and no automatic agent launch, so that secrets do not leak into the PTY and I stay in control.
78. As a user, I want `terminal.shell` to be unreadable and unwritable by agents, so that an agent cannot make my terminal execute arbitrary code.
79. As a user, I want `terminal.cwd` and `terminal.scrollback` readable and writable by agents, so that harmless preferences can be automated.
80. As a user, I want terminal output not logged and the process group killed on quit, so that secrets do not leak and agents do not linger.
81. As a user, I want the terminal to work when MCP is off, so that it is a normal terminal first.
82. As a user, I want terminal settings in Settings with clear “applies on restart” guidance, so that I know when changes take effect.

### User experience and integration

83. As a user, I want exposed connections marked in the explorer, so that I can see at a glance what agents can reach.
84. As a user, I want a discreet MCP status indicator in the status bar, so that I know when remote control is active.
85. As a user, I want a well-designed approval dialog with risk labels, a monospace statement and obvious keyboard actions, so that decisions are fast and safe.
86. As a user, I want toasts for agent configuration and snippet changes, so that I notice side effects without opening a log.
87. As a user, I want clear empty, loading, running, exited and error states in Settings and the terminal, so that I always know what to do next.
88. As a user, I want terminal keystrokes not to trigger editor shortcuts, so that typing into an agent is reliable.
89. As a user, I want structured tool errors without stack traces, so that agent failures are actionable.
90. As a user, I want the guide to include recipes, so that agents use the tools in the intended order.
91. As a user, I want the terminal, history and value panels to coexist, so that I can arrange my workspace.
92. As a user, I want the terminal resizable with a sensible minimum, so that it fits small windows.
93. As a user, I want no shell to start until I open the terminal, so that cold start and idle memory are unaffected.
94. As a user, I want the feature to work on macOS, Windows and Linux, so that agent access is not platform-specific.
95. As a user, I want documentation for setup, safety and troubleshooting, so that I can use agents confidently.
96. As a user, I want an agent not to move a connection to a less strict profile, so that automation cannot weaken safeguards.
97. As a user, I want transport-security driver fields to be tighten-only, so that an agent cannot disable TLS verification or host-key checks.
98. As a user, I want a reset of a tightened setting to be rejected, so that “reset” cannot be used to loosen a safeguard.

## Implementation Decisions

### Workspace and module boundaries

Two new workspace crates are introduced.

`sqlr-mcp` owns the MCP server, the protocol boundary, authentication, discovery, tool
routing, policy enforcement, the guide, and the `AgentHost` trait used for operations that
need the running application. It depends on `sqlr-core`, `sqlr-engine` and `sqlr-store`,
plus `rmcp`, `axum`, `serde`, `schemars` and `tokio`. It does not depend on `sqlr-ui` or
`sqlr-app`. Queries, catalog reads and snippet operations execute against the engine
directly; configuration writes, editor operations, approvals and terminal-independent UI
state go through `AgentHost`.

`sqlr-terminal` owns the PTY session, terminal parser, input encoding, screen snapshot and
the egui terminal widget. It depends on `egui`, `portable-pty` and `vt100`. It does not
depend on `sqlr-mcp` or `sqlr-app`. The application passes the theme palette and font. It
is a leaf crate with its own tests and a fake backend.

`sqlr-app` implements `AgentHost`, owns the MCP server lifecycle and Settings UI, mounts
the terminal panel, adds the toolbar/menu/palette/keymap actions and handles approvals.
`sqlr-engine` gains agent sessions and headless helpers. `sqlr-core` gains the
`AgentAccess` enum and new settings/action definitions. `sqlr-store` gains MCP token
storage and continues to own connections, profiles, settings, keymap and tasks.

### MCP transport, lifecycle and discovery

- The transport is MCP Streamable HTTP at a single `/mcp` endpoint. The protocol is
  implemented with `rmcp` 3.7, pinned to an exact version because the API is still moving.
  The HTTP layer is `axum` 0.8 with the minimal feature set; no TLS is used on loopback.
- The listener binds to `127.0.0.1` only, on a fixed configurable port. The default is
  `47821`. Binding to `0.0.0.0`, IPv6 wildcard addresses or a Unix socket is not offered.
- The server is **disabled by default**. Enabling or disabling it, changing the port,
  rotating the token, and observing status all happen in Settings → Agents. Enabling does
  not expose any connection: exposure is a separate per-connection decision.
- Starting the server: read the persistent token from the shared credential vault (generate one if
  absent), bind the listener, build the router and session manager, publish the discovery
  file, then report `Running`. If the port is busy or the keychain is unavailable, the
  server reports a precise `PortBusy` or `KeychainError` status, writes no discovery
  file, logs the failure and leaves the rest of the app running.
- Stopping the server: stop accepting requests, cancel in-flight tool calls, deny pending
  approvals, drop all MCP sessions and their agent sessions, remove the discovery file,
  then report `Stopped`. Disabling the server at runtime does the same. Quitting the app
  stops the server before the process exits.
- Changing the port restarts the listener. Active MCP sessions are dropped and pending
  approvals are denied. The Settings UI warns about this before applying the change.
- Rotating the token replaces its encrypted vault value, updates the in-memory verifier, drops
  all MCP sessions and their agent sessions, and leaves the listener bound. The Settings
  UI warns that clients must be updated with the new token.
- Discovery: a file named `mcp.json` in the data directory contains the URL, process id,
  protocol version, application version and start time. On Unix it is created with mode
  `0600`; on Windows it inherits a user-scoped ACL. It never contains the token. It is
  written atomically (temporary file, then rename), removed on stop, and a stale file is
  overwritten on the next successful start or removed when the server is disabled. If the
  data directory is unavailable, the server does not start.
- Session management: at most eight concurrent MCP sessions. A ninth connection receives
  a structured “too many sessions” error. An MCP session is idle when it has no in-flight
  tool call and no open result activity for 30 minutes; it is then closed, with all
  cleanup described below. Client name and version are captured from the MCP initialize
  request and attached to approvals and log entries.

### Local security

- Every HTTP request requires a bearer token in the `Authorization` header, including the
  initialize request. The comparison is constant-time. Missing or wrong tokens receive
  `401 Unauthorized` with `WWW-Authenticate: Bearer`.
- Tokens are generated with 256 bits of OS randomness, encoded as unpadded base64url,
  stored in the shared encrypted credential vault under a separate namespace not tied to a connection,
  and never written to TOML, the discovery file, logs or `Debug` output.
- Origin validation: a request with no `Origin` header is accepted (normal non-browser
  clients). An `Origin` is accepted only when it is `http://127.0.0.1:<port>` or
  `http://localhost:<port>` for the configured port; everything else, including `null`,
  receives `403 Forbidden`.
- Host validation: the `Host` header must be `127.0.0.1:<port>` or `localhost:<port>`.
  Everything else receives `403 Forbidden`. Per-request CORS preflight responses are not
  provided.
- There is no TLS. The transport is loopback-only and authenticated; adding certificates
  is out of scope.
- The server surfaces only MCP **tools**. No resources, prompts, sampling, elicitation or
  server-initiated requests are implemented.
- The initialize response carries short instructions telling the agent to call the guide
  first and to treat connection access as explicit consent.

### Settings, token and discovery keys

New settings:

| Key | Type | Default | Levels | Notes |
|---|---|---|---|---|
| `agents.enabled` | bool | `false` | global only | Starts/stops the MCP server |
| `agents.port` | int, 1024–65535 | `47821` | global only | Changing restarts the server |
| `terminal.shell` | text | empty | global only | Empty means automatic; human-only, unreadable and unwritable by agents |
| `terminal.cwd` | text | empty | global only | Empty means `terminal/` inside the app's configuration directory |
| `terminal.font_size` | float, 10–24 | `12` | global only | Terminal text size in points |
| `terminal.scrollback` | int, 100–10000 | `2000` | global only | Lines retained by the terminal |

`agents.*` settings are readable by agents but never writable. `terminal.shell` is neither
readable nor writable by agents. `terminal.cwd`, `terminal.font_size` and
`terminal.scrollback` are readable and writable, subject to normal setting validation.
None of these keys is exposed at profile or connection level.

The MCP token has no setting row. Its lifecycle is an explicit Settings action. The
Settings UI shows whether a token exists and confirms rotations; it does not store a token
timestamp or any other token metadata.

### Agent access model

`AgentAccess` is a three-value enum: `None`, `Read`, `Write`, with `None` as the default.
It is stored on the connection configuration and serialized with a default so that
existing `connections.toml` files remain valid. The value is changed only by the person,
through the connection dialog or the explorer context menu. An agent-created connection
has `None`; a duplicated connection resets to `None`. The agent write path preserves the
existing value and rejects attempts to set it.

- `Read`: the connection may be browsed and queried only with statements that the relevant
  classifier considers reads. The agent session is put into read-only mode on the server
  and the classifier is a second line of defence. Any modifying statement or command is
  rejected with `read_only` before execution.
- `Write`: the connection may be browsed and queried with modifying statements. The
  connection profile still decides commit mode, read-only mode, confirmation of writes and
  confirmation of destructive statements. Exposure and profile are cumulative; neither
  bypasses the other.
- Changing exposure while an agent session is open takes effect on the next session;
  existing sessions are not silently converted. Disabling exposure (`None`) is only
  effective for new sessions, but the UI disconnects agent sessions for that connection
  when the value becomes `None`.
- Connections with `None` do not appear in agent-facing connection lists or catalog tools;
  a tool call that names them returns `not_exposed`.

### Sessions, transactions and connection lifecycle

- The engine gains an agent session slot in addition to the metadata/explorer slot and the
  per-editor-tab slot. An agent session is keyed by MCP session and connection, so two MCP
  clients never share a transaction, and an agent never shares a transaction with an editor
  tab.
- The metadata session remains the owner of explorer catalog state. When an agent needs a
  connection that is not connected, the application ensures the metadata session is
  connected (so the explorer reflects the connection) and opens the agent session for the
  actual work. The application records whether it opened the metadata session on the
  agent's behalf.
- Connections open lazily on the first catalog read or query. There are no `connect` or
  `disconnect` tools: an agent must not be able to tear down a connection the person is
  using.
- If credentials are missing (for example `save_secrets` is off and no in-memory secret
  exists), the tool returns `secret_required`, naming the connection and the secret field
  keys but not their values. The agent may write the secret with the configuration tools.
- On MCP session end, idle timeout, token rotation, server stop or application quit: cancel
  in-flight queries, close the session's results, close every agent session (rolling back
  open transactions), and close the metadata session if the application opened it only for
  this agent. A connection that the person already had open remains open.
- Agent sessions follow the connection profile exactly: commit mode, savepoints, read-only
  flag and idle transaction timeout. A read-exposure session is forced read-only regardless
  of profile.
- Paged results read inside an open transaction, as the editor's paged results do.
- The agent session's active query is registered in Operations with its connection, kind,
  target, elapsed time, progress and a Cancel action. Completed agent queries are recorded
  in History like any other query. There is no separate “agent” marker in History in this
  version.

### Approvals

Approvals are the same safety mechanism the person already uses, presented to an agent
instead of a keystroke.

- A query is classified per family: SQL uses the dialect classifier and script classifier,
  MongoDB uses the document shell classifier, Elasticsearch uses the document request
  classifier, Redis uses the key-value command classifier.
- If the session is read-only, modifying statements are rejected without an approval.
- If the profile requests confirmation for writes (`confirm_writes`) or destructive
  statements (`confirm_destructive`), the tool call blocks and the application shows a
  modal approval dialog. The dialog contains:
  - the client name/version and MCP session identifier;
  - connection name, profile, database and exposure level;
  - a risk label: Write, Destructive, Configuration delete or Snippet delete;
  - the exact statement or command in a scrollable monospace area, without truncation
    hidden behind a tooltip;
  - the target object for configuration deletes, including the number of contained items
    where relevant;
  - a countdown for the approval timeout.
- Approvals time out after 60 seconds and return `denied` with reason `timeout`. Denying
  returns `denied`. If the client disconnects, the application quits or the server stops,
  the approval is denied immediately.
- The dialog defaults to the safe action: `Deny` has initial focus, `Escape` denies, and
  the person must deliberately choose `Allow`. There is no “remember for this session”
  option for destructive statements.
- A script run that needs confirmation follows the human runner's behaviour: the approval
  shows the statement or the full script that will run, so the person sees the exact unit
  that is being authorised.
- Multiple approval requests are queued and shown one at a time. Each request keeps its
  own timeout.
- Configuration deletes and snippet deletes always require approval, independently of
  query confirmations.
- Secret writes never require approval. Secret writes and removals are logged with field
  names only.

### Configuration write model and safety floor

Connection values are write-only. `config_list` returns only `id`, `name`, `driver`,
folder, profile id and agent access for a connection, plus a `secret_fields` map that says
whether each secret field has a stored value. Hosts, ports, users, TLS/SSH options, driver
properties and secret values are never returned. Driver manifests are readable so that an
agent can know the field keys, types, defaults and secret flags needed to create a
connection.

Profiles, settings, keymap and tasks are readable because they are policy and definitions,
not credentials. This lets an agent see `read_only`, confirmation flags, current setting
values and task definitions and avoid breaking them.

The following are **human-only** and cannot be read or written through the configuration
tools unless stated otherwise:

- installing, removing or rolling back drivers;
- the MCP server settings (`agents.enabled`, `agents.port`) and token lifecycle;
- application updates;
- `terminal.shell` (not even readable).

Safety flags are **tighten-only** for agents. A profile or setting that is already
read-only cannot be made writable; a confirmation that is on cannot be turned off; a
safety setting cannot be relaxed. The existing `Safety::TightenOnly` notion for settings
is extended to profile safety fields and to connection-level safety overrides. A request
that would loosen a flag is rejected with `invalid` and a message naming the flag.

The same rule closes three related loopholes:

- changing a connection's profile is allowed only when the target profile is at least as
  strict as the current one on read-only, write confirmation, destructive confirmation and
  idle-transaction timeout; an agent cannot route a connection to a more permissive
  profile;
- resetting a tighten-only setting that is currently tightened is rejected, because the
  reset would loosen it;
- driver fields that weaken transport security (TLS verification, certificate checks, SSH
  host-key checking and similar) are marked tighten-only in the driver manifest, and a
  patch that relaxes one is rejected.

Configuration changes are applied through the running application, never by editing
files directly. Connection and folder changes go through the existing store worker;
profile, setting, keymap and task changes go through the existing in-memory stores and
their save paths, so the UI and the files stay consistent. An agent write updates the
running UI immediately and is acknowledged only after the store confirms it.

Patches are partial: only provided fields change. `remove_values` and `remove_secrets`
name keys to delete; omission leaves a key untouched. Connection `driver` is immutable
after creation. A patch never changes `agent_access`.
Writes are serialized by the application; per field, the last write wins, and fields not
mentioned by the patch are untouched. `save_secrets` keeps its existing meaning: secrets
for a connection with it off are kept only in memory for the session and are not written
to the credential vault or to TOML.

A connection's secret presence is metadata; its value is not. `config_list` reports, for
each secret field declared by the driver, whether a value is stored. A `secret_required`
error names the field keys, never their values.

### MCP tool surface

The server exposes 19 tools. All tool results are MCP `CallToolResult`s with text content
for the guide and structured content for the rest. Tool names and schemas are stable; the
guide is generated from the same registry so it cannot drift.

| Tool | Request | Response and semantics |
|---|---|---|
| `get_guide` | optional `topic`: `overview`, `security`, `tools`, `recipes`, `terminal` | Markdown guide for the requested topic, or the overview by default. Includes the app model, exposure, safety, limits, tool catalogue, recipes and a note that the terminal is not accessible via MCP. Generated from the tool registry. |
| `config_list` | `target`: `connections`, `folders`, `profiles`, `settings`, `keymap`, `tasks`, `drivers`; optional `driver_id` | Metadata and definitions. Connections return only metadata, including a `secret_fields` map of key to “stored or not” and never a value. Settings return definitions plus current global values and stored profile/connection overrides, with `terminal.shell` omitted. Drivers return manifests with fields and secret flags. No secrets anywhere. |
| `config_write` | `target`: `connection`, `folder`, `profile`, `setting`, `keymap`, `task`; target-specific payload | Create or patch. Connection payload supports create/update, value patch, value removal, secret write/removal and `save_secrets`; `agent_access`, `driver` changes and protected keys are rejected. Folder supports create/rename/move. Profile supports create/update with tighten-only safety. Setting supports set/reset with global/profile/connection level, validated by the settings catalogue. Keymap supports set/reset per action. Task supports create/update; tasks cannot be run. |
| `config_delete` | `target`: `connection`, `folder`, `profile`, `task`; `id` | Deletes after an approval. A connection is disconnected and its sessions and secrets are removed. Deleting a folder removes the folder and moves its connections and subfolders to the parent; no connection is deleted. A profile in use is rejected. A task is deleted but never executed. |
| `browse` | `connection_id`, `path` (array of path segments) | Lazily loads and returns child nodes with name, kind, path, child flag and detail; includes `from_cache`. Works for every database family. |
| `describe_object` | `connection_id`, optional `database`, `schema`, `name` | Relational table details: columns, types, nullability, defaults, primary key, indexes and foreign keys. Other families return `unsupported`; `browse` remains the generic path. |
| `search_objects` | `query`, optional `connection_id`, optional `limit` | Searches the catalog of exposed connections, with a result limit. Returns objects with connection and path, plus `from_cache`. |
| `run_query` | `connection_id`, `query`, optional `database`, optional `max_rows`, optional `timeout_s` | Runs exactly one statement or command, family-aware. Synchronous up to the deadline. Returns a tagged result with a page, `truncated`, `query_id` and status. |
| `get_query_result` | `query_id`, optional `offset`, optional `limit` | Pages an existing result, fetching more from the server as needed, with the same caps and TTL. Expired or unknown ids return `not_found`. |
| `cancel_query` | `query_id` | Requests cancellation through the engine. Idempotent; a finished query reports its final state. |
| `close_result` | `query_id` | Releases the result and its memory. Idempotent. |
| `list_query_tabs` | none | Open SQL query tabs only: id, title, connection id, database, dirty flag, revision. Object, DDL, admin, document and Redis tabs are excluded. |
| `get_query_tab` | `tab_id` | Text, revision, dirty flag, connection, database, caret and selection in character offsets. |
| `edit_query_tab` | `tab_id`, `expected_revision`, edit | Replaces the whole text or applies a list of range edits. Rejects on a revision conflict. One undo step; visible highlight and scroll; tab becomes dirty. No save, no tab creation. |
| `run_query_tab` | `tab_id`, `target`: `script`, `statement` with index, or `selection`; optional `max_rows` | Runs in the tab's session and database, populates the tab's grid and returns a bounded per-statement summary plus the query id where paging applies. Rejects a busy tab, an unexposed connection or a read-only violation. |
| `list_snippets` | none | Snippet metadata: id, name, folder, connection scope, updated time. |
| `get_snippet` | `id` | Full snippet, including SQL. |
| `write_snippet` | `id` optional, `name`, `folder`, `conn_id` optional, `sql` | Creates or updates; moves by changing folder or connection scope. Returns the id. |
| `delete_snippet` | `id` | Deletes after an approval. |

Tool names are flat and not prefixed, because they are scoped to the sqlr MCP server.

Configuration payload details:

- `connection`: `id` is absent on create and present on update. Create requires `name`
  and `driver`; update requires `id`. Fields: `name`, `group`, `color` (an RGB triple),
  `values`, `remove_values`, `profile`, `save_secrets`, `secrets`, `remove_secrets`.
  `values` and `secrets` are maps of field key to string; `remove_values` and
  `remove_secrets` name keys to delete. A patch containing `agent_access` or a `driver`
  different from the existing one is rejected, not ignored.
- `folder`: `action` is `create`, `rename` or `move`. `create` takes `name`; `rename`
  takes `from` and `to`; `move` takes `name` and `target`, where `target` is
  `{ "at": "end" }`, `{ "at": "before", "id": "..." }`,
  `{ "at": "after", "id": "..." }` or `{ "at": "into", "id": "..." }`.
- `profile`: `id` absent on create, present on update. Fields: `name`, `color`,
  `commit_mode`, `confirm_writes`, `confirm_destructive`, `read_only`,
  `idle_tx_timeout_min`. Safety fields are tighten-only.
- `setting`: `id`, `value` or `reset`, and `level`: `global`, `profile` with `id`, or
  `connection` with `id`. The value is validated against the setting definition and its
  allowed levels; `terminal.shell`, `agents.*` and other protected keys are rejected.
- `keymap`: `action` and `chords`, or `reset` for one action. Chords are validated by the
  keymap parser. All actions, including the terminal toggle, are remappable.
- `task`: `id` absent on create, present on update, plus the task definition. Tasks can be
  read and written but never run through MCP.

`config_list` shapes:

- `connections`: `id`, `name`, `driver`, `group`, `profile`, `agent_access` and
  `secret_fields` (field key to stored/missing).
- `settings`: one entry per setting with id, section, label, description, kind, default,
  allowed levels, safety, and a `values` map keyed by `global`, `profile:<id>` and
  `connection:<id>`. `terminal.shell` has no value entry.
- `keymap`: per action id, the current chord list and whether it differs from the preset.
- `tasks`: full task definitions, with connection ids but without secrets.
- `drivers`: manifests with field keys, labels, types, defaults, required flags, secret
  flags, tighten-only flags, groups and capabilities.

### Query contract and limits

`run_query` executes exactly one statement or native command. For SQL, the dialect must
split the input into one statement; more than one returns `invalid` with the number of
statements and a hint to use an editor tab for scripts. MongoDB accepts one shell
statement, Elasticsearch one console request or query, and Redis one command line.

SQL runs on the agent session for the connection and database. MongoDB and Elasticsearch
use the engine's document execution path on the agent session. Redis uses a dedicated
agent connection and does not support `MULTI` blocks, `MONITOR`, `SUBSCRIBE` or blocking
commands in this version; those return `unsupported` rather than hanging.

Limits:

- The tool waits up to 30 seconds by default; `timeout_s` may shorten or lengthen this up
  to 120 seconds.
- A response contains at most `max_rows` rows (default 200, maximum 1000) and at most
  256 KiB of encoded row data. When either limit is reached, `truncated` is true and the
  `query_id` remains available for paging.
- If the query is still running when the deadline expires, the result reports
  `status: "running"` with the rows fetched so far and a `query_id`.
- Results remain available for five minutes after the last access. A background sweep
  closes expired results. `close_result` closes immediately. Closing an MCP session
  closes all of its results.
- Results count against the engine's existing per-result and global memory budgets; an
  agent query never triggers “fetch all”.

The result is a tagged union:

```json
{
  "kind": "tabular | document | reply",
  "status": "ready | running | cancelled | failed",
  "query_id": "opaque string",
  "elapsed_ms": 0,
  "truncated": false,
  "error": { "code": "string", "message": "string" },
  "tabular": {
    "columns": [{ "name": "string", "type_name": "string", "kind": "string", "hint": "string" }],
    "rows": [[null, true, 1, 1.5, "text", "base64"]],
    "affected": null,
    "messages": [{ "level": "info | notice | warning | error", "text": "string" }]
  },
  "document": { "documents": [] },
  "reply": { "value": null }
}
```

Only the member matching `kind` is present. Value mapping:

- `null` stays JSON `null`; a text value `"NULL"` is a different value.
- Booleans and integers are JSON booleans and numbers.
- Finite floats are JSON numbers; `NaN` and infinities are strings `"NaN"`, `"Infinity"`
  and `"-Infinity"` so the JSON remains valid.
- Exact decimals (`DisplayHint::Decimal`), temporal values, intervals, UUIDs, XML,
  geometry and arrays are strings exactly as displayed by the result grid, preserving
  precision.
- Binary values are base64 strings. The column hint tells the caller that the column is
  binary.
- JSON columns are parsed into JSON values when valid, otherwise returned as strings.
- Document results are JSON documents. Tabular document queries use the tabular shape.
- Redis replies are JSON representations of RESP values: nil, string, integer, array and
  error.

`get_query_result` uses the same page limits and returns the same shape plus the requested
offset. `cancel_query` and `close_result` accept any known id and are idempotent.

### Editor contract

The editor tools operate only on SQL query tabs. They may be used on any open tab
regardless of exposure; only running requires exposure. The agent cannot create, close or
save a tab.

`get_query_tab` returns character offsets (Unicode scalar values, end exclusive) for the
caret and selection. `edit_query_tab` supports two edit forms:

- `replace` with the complete new text;
- `edits` with a list of `{ start, end, text }` ranges, applied as one change set.

Both require `expected_revision`. If the editor's current revision differs, the call returns
`conflict` with the current revision and makes no change. The agent can re-read and retry.
An edit is a single undo group; the tab becomes dirty; the editor scrolls to and briefly
highlights the first changed range. The person's undo reverts the entire agent edit in one
step. The agent cannot save the tab to a file.

`run_query_tab` runs the tab's current buffer (not a stale saved version):

- `script` runs the whole buffer as a script, like Run Script;
- `statement` with a zero-based index runs one split statement;
- `selection` runs the current selection, or returns `invalid` if there is none.

It uses the tab's session and server database, applies the same profile confirmations and
read-only checks as the human UI, records History, and places results in the tab's normal
result area. The tool returns a bounded summary of the run. If the tab is already running,
the call returns `busy` and does not interrupt the user's run. A tab whose connection is
not exposed returns `not_exposed`. A tab with no connection returns `invalid`.

### Snippet contract

Snippets are readable and writable in full. `list_snippets` returns metadata only;
`get_snippet` returns SQL. `write_snippet` creates when no id is given and updates
otherwise; name, folder and connection scope are patchable; folders are `/`-separated
strings and are created implicitly. `delete_snippet` requires an approval and reports the
name and folder in the dialog. Snippets may be global or scoped to a connection.

### Guide contract

`get_guide` returns a Markdown document that is generated from the same registry that
produces the tool list. It covers:

- what sqlr is and the difference between the running application and a headless CLI;
- the exposure model and the meaning of `None`, `Read` and `Write`;
- the write-only connection model and the rules for secrets;
- approvals and the types of operations that can require them;
- the tool catalogue with one-line descriptions and required/optional arguments;
- recipes: discover schema, write a query, page results, fix an open tab, save a snippet,
  create a connection, and set up an agent inside the terminal;
- limits: row/byte caps, deadlines, result TTL, session cap and idle timeout;
- a note that the integrated terminal exists but is not accessible through MCP.

A test asserts that every registered tool appears in the guide. The guide contains no
secrets, connection values or environment-specific data.

### Integrated terminal architecture

The terminal is a real PTY, not a command runner. It uses `portable-pty` for the PTY and
process, and `vt100` for parsing and screen state. The widget is custom because the
available egui terminal crates do not support the pinned egui version.

- A terminal session owns the PTY master, the child process, a reader thread, a writer
  thread and the parser state. The UI thread only reads a screen snapshot and sends input,
  resize and control messages; it never blocks on the PTY.
- The terminal is spawned lazily on the first open. Before that, the panel exists only as
  hidden state; cold start is unaffected. Hiding the panel does not kill the process. The
  shell keeps running so an agent can continue its task.
- Default shell: on Unix, the user's `$SHELL` with login and interactive behaviour
  (`-l`); on Windows, `pwsh.exe` when available, then `powershell.exe`, then `cmd.exe`.
  A configured shell command is run through the platform shell wrapper so arguments and
  quoting work. The working directory defaults to `terminal/` inside the app's
  configuration directory. It is stable across app launches and is prepared on a
  background worker before the shell starts. An explicit `terminal.cwd` is respected.
- The default terminal directory contains `AGENTS.md`, created only if missing, with
  MCP setup instructions and the required `get_guide` workflow. `.mcp.json` contains
  the active server's client configuration, including the bearer token, and is
  refreshed atomically when the server starts, stops, changes port or rotates its
  token, including while the terminal is hidden. When MCP is disabled, it contains
  an empty `mcpServers` object. Existing identical configuration is not rewritten.
  On Unix the directory is created with mode 0700 and `.mcp.json` with mode 0600.
  Both files and user-created scratch files persist when sqlr closes. User edits to
  `AGENTS.md` are preserved. No files are generated in an explicit `terminal.cwd`.
- Environment: inherit the application environment, set `TERM=xterm-256color` and
  `COLORTERM=truecolor`, and do not inject MCP tokens or agent-specific variables.
- Terminal settings are global: shell command, working directory, font size and
  scrollback. Changes apply to the next Restart; the running session is not killed
  implicitly. Settings show that behaviour. The font size applies immediately.
- Rendering: only visible rows and the active scrollback window are painted. Cells are
  batched by style; the font is the app's monospace font and the ANSI palette is derived
  from the active theme. Support bold, dim, italic, underline, reverse video, 16-colour,
  256-colour and truecolour; cursor styles; wide characters; combining characters;
  alternate screen; scrollback; and resizing with PTY size updates.
- Interaction: drag selection, double-click word selection, copy and paste with
  platform-appropriate shortcuts, bracketed paste, mouse-wheel scrollback and a visual
  bell. `Ctrl+C` is always the interrupt when the terminal has focus; copy uses `Cmd+C`
  on macOS and `Ctrl+Shift+C` elsewhere; paste uses `Cmd+V` or `Ctrl+Shift+V`. Mouse
  reporting to applications and clickable hyperlinks are not implemented.
- Focus: clicking the terminal focuses it. While focused, editor and global query
  shortcuts are suppressed so keystrokes reach the shell, except the global terminal
  toggle, Settings and other explicitly global actions. Toggling the panel focuses the
  terminal.
- Lifecycle: the panel header offers Restart, Clear, Kill and Copy MCP config. Kill
  terminates the process group after a short grace period. Restart creates a new session
  with the current settings. When the process exits, the panel keeps the final screen and
  shows the exit status with a Restart action. On application quit, the process group is
  killed; no shell or agent process is left behind.
- Logging: terminal output, scrollback and the shell command are never logged. Only
  lifecycle metadata (started, exited with code, killed) may be logged, without the
  command or working directory.
- Security boundary: the terminal is human-only. No MCP tool can read it, write to it,
  start or stop it. The agent launched inside is an ordinary external MCP client and is
  subject to the same exposure, approvals and logging as any other client.
- `terminal.shell` is human-only and unreadable through MCP. `terminal.cwd`,
  `terminal.font_size` and `terminal.scrollback` are normal settings, readable and
  writable by agents. The shell command is never included in `config_list`, logs,
  diagnostics or error messages.

### Application integration and threading

- `AgentHost` is an async trait with methods for connection metadata, configuration
  writes/deletes, editor tab operations, approvals and copying the MCP client
  configuration. The application implements it with a request channel and one-shot
  responses. A request that needs the UI thread is enqueued, wakes the UI with
  `request_repaint`, and is processed at the start of the next frame. The UI thread never
  performs I/O or waits for a database.
- Configuration writes use the existing store worker and in-memory stores. The host keeps
  a pending map keyed by request id so that the store's reply resolves the MCP call with
  the actual result and refreshed state. A timeout returns `timeout` without applying a
  half-written change.
- Queries, catalog reads and snippet operations execute against the engine on the tokio
  runtime. Approvals and editor operations go through the host. Snippets persist through
  the existing library store.
- The MCP server starts on the engine runtime and is owned by the application. It holds a
  cancellation handle for shutdown. The application checks the server status each frame
  for Settings and the status bar.
- Editor edits are applied through a new atomic editor API: replace-all or a validated
  edit list as one undo group, with revision checks and caret adjustment. The existing
  editor remains the owner of text, carets, markers and undo history.
- The terminal panel is owned by the application but its session and widget state live in
  the terminal crate. The application passes theme, font size, UI scale and the current
  settings; the terminal returns input events to be sent to the PTY.

### User interface

The UI is intentionally small and integrated rather than a new workspace.

**Settings → Agents**

- A clear enable switch with a short explanation of what enabling means: a local endpoint
  for agents, no connection exposed yet, token required.
- Port field with validation and a note that changing it restarts the server.
- A status pill: Stopped, Starting, Running, Port busy, Keychain error, Error. Running shows
  the endpoint and active session count.
- Endpoint row with a copy button. Token row with a “Rotate token” action and a warning
  that active agents will be disconnected. “Copy MCP config” copies a ready-to-paste
  client configuration and warns that it contains the token.
- A short “How exposure works” paragraph with a link to the usage guide.
- Last error, if any, in a readable inline message rather than a toast that disappears.

**Settings → Terminal**

- Shell command, working directory (text plus a folder picker), font size and scrollback.
- A note that shell, directory and scrollback changes apply on Restart; the font size
  applies immediately.
- A note that the shell command is not visible to agents and is never logged, and that it
  is stored like any other setting, so credentials should not be embedded in it; use the
  shell's own mechanisms instead.
- A button “Open terminal” that focuses the panel.

**Connection dialog and explorer**

- A compact “Agent access” segmented control in the Main tab of the connection dialog,
  below the profile/colour row, with icons and explanatory text for None, Read and Write.
  Write carries a warning that profile confirmations still apply. The control is human-only
  and is never shown or applied on the agent write path.
- The explorer marks exposed connections with a small, muted badge (for example a robot or
  eye icon) and a tooltip “Agents: read” or “Agents: read and write”. The badge must not
  compete with connection colour or transaction marks.
- The explorer context menu offers an “Agent access” submenu for quick changes, using the
  same store path as the dialog.

**Approval dialog**

- Centered, modal, with a clear title (“Agent request”), the client name and version, the
  connection and database, a risk badge and the exact statement in a scrollable monospace
  block.
- `Deny` is focused by default. `Escape` denies. The allow action is visually primary but
  not preselected for destructive requests. The countdown is visible and does not cause
  layout shifts. The dialog explains that the request will be denied at timeout.
- For configuration deletes, the statement area is replaced by a target summary and the
  consequences: a connection is disconnected and loses its saved secrets; a folder's
  connections and subfolders move to the parent; a profile in use is rejected before the
  dialog; a task is removed but never run.
- No “remember” option is shown.

**Status bar**

- When MCP is enabled, a discreet segment shows a status dot, “MCP” and the number of
  active sessions; clicking it opens Settings → Agents. It is hidden when MCP is off and
  never takes space from the connection/transaction segments on narrow windows.

**Terminal panel**

- Bottom panel directly above the status bar; history sits above it when both are open.
  Default height about 300 px, minimum 120 px, resizable with the standard splitter.
- Header: a terminal icon and title, a status dot, the shell/cwd summary, and actions
  Restart, Clear and Copy MCP config (the same configuration content and token warning as
  Settings), with Kill as a clearly separated secondary action. The header uses the same
  spacing and typography as other panels.
- Empty/start state: the panel opens and starts the shell; if starting fails, it shows a
  concise error, a Retry action and a link to Terminal settings.
- Exit state: final screen remains, exit code and Restart are visible.
- The terminal canvas uses the theme background, foreground, selection colour and ANSI
  palette; no hard-coded colours. It supports the app's UI scale and monospace font.
- The toolbar toggle sits next to the value-panel and history toggles, with an active
  state and the tooltip “Terminal (Ctrl+Backtick)”. The action is also in View, the command
  palette, the shortcuts dialog and the keymap.

**Feedback**

- Agent configuration and snippet writes produce short toasts naming the target, for
  example “Agent updated connection Analytics”. Secret values, shell commands and result
  rows never appear in toasts.
- Errors use the existing error presentation patterns and never show stack traces.
- Focus states, hover tooltips and disabled states follow the existing design system.

### Persistence and schema

- `ConnectionConfig` gains `agent_access` with a serde default of `None`. Existing TOML
  files load unchanged; new files write the field. Duplicating a connection resets it.
- The MCP token is stored in `secrets.vault` in the config directory, not in TOML or SQLite.
  The OS keychain protects the vault's encryption key, cached once per application session
  and shared with connection credentials. The token is not included in `Debug` output or
  diagnostics. The legacy `sqlr-mcp` / `token` keychain item migrates on first use: persist
  the encrypted token before removing the original and retry pending cleanup after failure.
- No new SQLite tables are added. History and snippets keep their existing schema; agent
  queries are ordinary history entries. There is no persistent audit table.
- The discovery file is a runtime artifact, not user configuration, and is removed on
  shutdown.
- Terminal visibility and height are session-only. Terminal settings are persisted in the
  settings file like other settings.

### Logging and observability

- Agent operations are logged through `tracing` with the client name/version, MCP session
  id, tool name, target kind and id, outcome, duration and numeric result metadata such as
  row count or query id. The log never contains SQL text, native commands, result rows,
  secrets, connection values, the terminal shell command or terminal output.
- Configuration writes log target and changed field names only. Editor edits log tab id,
  revision and edit count. Approvals log decision and risk class.
- The default filter keeps `rmcp` at warn level so request bodies are not logged, while
  sqlr's own MCP target is at info.
- Errors are sanitized: passwords, tokens, URLs with embedded credentials and secret
  values are redacted before they reach a log, a toast or an MCP error.
- No persistent audit trail is written. The rotating application log is the record. This
  is a deliberate trade-off and is documented in the usage guide.
- Query History remains available for queries; no agent marker is added in this version.
  The Operations panel lists in-flight agent queries with the same cancel controls as
  human queries.

### Dependencies and platform notes

- `rmcp` is pinned to an exact 3.7 version; `schemars` is aligned with the version rmcp
  uses. `axum` uses minimal features, no TLS features and no OpenSSL. `tokio` uses the
  already-enabled runtime, sync, net, time and io features.
- `portable-pty` 0.9 and `vt100` 0.18 are pinned. No second egui version is introduced.
  No OpenSSL dependency is added. Binary size and cold start are measured against the
  product budgets.
- macOS and Linux use the user's login shell, `0600` on the discovery file, and process
  group termination for the terminal. Keychain access uses the existing secret store.
- Windows uses ConPTY through `portable-pty`, PowerShell/cmd detection, a user-scoped ACL
  for the discovery file and Credential Manager for the token. The loopback listener and
  MCP protocol are identical.
- The terminal and MCP server are local-only features and work with no network access
  beyond loopback.

### Documentation deliverables

- A section in the product specification describing agent access and the integrated
  terminal, with the security model and the exposure rules.
- A technical document covering the MCP transport, tool contracts, limits, error codes
  and the `AgentHost` boundary.
- A usage guide for enabling MCP, configuring a client, exposure, approvals, token
  rotation, the terminal, Copy MCP config, troubleshooting port/keychain errors and the
  no-audit trade-off.
- Updates to the settings and shortcuts documentation and to the workspace crate table.

### Implementation order

1. Core/store/engine plumbing: `AgentAccess`, connection serialization, settings keys,
   token storage, `Slot::Agent`, headless query/catalog helpers, agent session cleanup,
   read-only enforcement.
2. `sqlr-mcp` transport: lifecycle, auth, Origin/Host, discovery, session management,
   status, guide and protocol tests.
3. Configuration, catalog and query tools with policy, approvals and result contracts.
4. Editor and snippet tools.
5. `sqlr-terminal`: PTY session, parser, input, widget, settings and tests.
6. Application integration: `AgentHost`, Settings, connection exposure, approval dialog,
   status bar, terminal panel, menu/palette/keymap, toasts.
7. Documentation and performance/binary-size validation.

## Testing Decisions

Good tests describe external behaviour, not implementation details. They drive the real
seam, assert on the public contract and observable UI, and avoid timing assumptions,
private structs, log strings and internal call counts. Timeouts are polled with a deadline,
not fixed sleeps. No test writes a real secret to a fixture, snapshot, log or assertion
message. The test that checks redaction inspects the produced text rather than relying on
the implementation.

The feature has **two new seams** and reuses the existing ones:

1. **MCP protocol boundary.** Start the real `sqlr-mcp` server on an ephemeral test port,
   with a real `Engine` over SQLite and the fake driver, and a scripted `AgentHost` for
   operations that normally need the UI. Drive it with an in-process MCP HTTP client. This
   covers authentication, headers, initialize, `tools/list`, `tools/call`, exposure,
   read-only enforcement, approvals, caps, paging, TTL, cancellation, structured errors,
   config rules, editor revisions and the guide. It is the highest seam for the MCP
   feature and the main regression net.
2. **Terminal facade.** Exercise the terminal session and widget through a fake PTY
   backend for screen parsing, input encoding, resize, scrollback, selection, copy/paste,
   bracketed paste, exit, restart and kill. Add a single real-PTY smoke test that runs a
   short platform command (`sh -c` or `cmd /c`) where a PTY is available. This seam keeps
   terminal tests fast and deterministic while still proving the real PTY path.

Existing seams reused:

- The `egui_kittest` application harness for Settings, the connection dialog exposure
  control, the explorer badge, the approval dialog, the status bar, the terminal panel,
  focus behaviour, toasts and keyboard actions.
- The engine integration tests for agent sessions, transaction isolation, rollback,
  read-only enforcement, headless document/key-value queries, result caps and cancellation.
- The store tests for `AgentAccess` serialization and backward compatibility, settings
  validation, keymap writes, task writes and token storage with the memory secret store.
- The UI/widget tests for rendering primitives; screenshots are used only where visual
  state is the behaviour under test, not as a substitute for behavioural assertions.

Test doubles: `FakeDriver` with scripted results; SQLite in-memory; `MemorySecretStore`; a
scripted `AgentHost`; a fake PTY backend; temporary data directories; an ephemeral port for
the server (test-only, not a user setting); and a test keymap/theme.

Modules and prior art:

- `sqlr-mcp` gets protocol, policy, lifecycle and guide tests, modelled on the existing
  engine integration tests and how they construct an engine with a fake driver.
- `sqlr-engine` gains agent-session and headless-query tests next to the existing session,
  transaction and result tests.
- `sqlr-app` uses the existing `egui_kittest` harness patterns (`Harness`,
  `build_ui_state`, `step_until`, the fake driver and the memory secret store) for all UI
  behaviour.
- `sqlr-terminal` gets unit tests for key encoding, colour mapping, screen snapshots,
  wide characters, selection and shell resolution, plus the fake-backend and real-PTY
  integration tests.
- `sqlr-store` follows the existing connection/settings/keymap/task test style.

Required test coverage:

**Transport and security**

- Disabled by default: no listener and no discovery file.
- Enable: binds loopback, writes a `0600` discovery file without a token, reports Running.
- Disable and quit: removes the file, stops the listener, denies pending requests.
- Port busy: status, error, no file, app continues; changing the port retries.
- Stale discovery file is replaced or removed correctly.
- Missing token returns 401; wrong token returns 401; correct token succeeds.
- Token rotation invalidates the old token and drops sessions.
- Origin/Host matrix: no Origin accepted; loopback origins accepted; `null`, remote and
  arbitrary origins rejected with 403; non-loopback Host rejected.
- Session cap and idle timeout clean up sessions and agent sessions.
- Keychain failure disables the server with a clear status and no secret leak.
- `tools/list` contains exactly the documented tools; the guide mentions every one;
  initialize instructions are present.

**Exposure and sessions**

- New connections default to `None`; duplicate resets to `None`; agent writes cannot change
  exposure.
- `None`: connection absent from lists and every data tool returns `not_exposed`.
- `Read`: SQL, Mongo, Elasticsearch and Redis modifications are rejected, including
  statements hidden in scripts and CTEs; session is server-read-only.
- `Write`: profile confirmations and read-only still apply.
- Lazy connect opens metadata and agent sessions; explorer status reflects the connection.
- No `connect`/`disconnect` tool exists.
- Two MCP sessions on one connection have separate transactions; an editor tab has a third
  transaction.
- Closing an MCP session rolls back its open transaction and closes its results; a
  user-opened metadata session remains.
- Missing secret returns `secret_required` with field names only.

**Approvals**

- Destructive SQL, document, key-value and configuration deletes trigger the dialog.
- The dialog shows client, connection, database and the exact statement.
- Allow executes; Deny returns `denied`; no response returns `denied` with `timeout`.
- Client disconnect and app quit deny immediately.
- Read-only or non-exposed sessions never reach an approval.
- Secret writes do not prompt.
- Approval queue and one-at-a-time behaviour.
- No response path bypasses the profile; tests compare an agent call with the equivalent
  human run.

**Configuration**

- Connection metadata contains no values or secrets; secrets never appear in any result,
  error, log or guide.
- Create and patch: partial patch preserves omitted fields; remove lists delete values;
  driver is immutable; `agent_access` is rejected.
- Secret write/removal is acknowledged but never readable; fields are logged by name only.
- Profiles, settings, keymap and tasks are readable and writable; validation errors are
  structured.
- Tighten-only rejection for read-only and confirmation flags, at profile and setting
  level; resetting a tightened setting is rejected; moving a connection to a less strict
  profile is rejected; relaxing a tighten-only driver security field is rejected.
- Protected keys (`agents.*`, `terminal.shell`) are rejected or absent; `terminal.shell`
  is not readable.
- Configuration deletes require approval; a profile in use is rejected; connection delete
  disconnects sessions and removes secrets; folder semantics match the UI; tasks are never
  executed.
- Writes are applied to the running app and are visible in the UI before the tool returns.

**Queries, results and catalog**

- SQL one-statement enforcement; multi-statement returns a helpful error.
- SQLite/fake-driver query returns typed columns and rows; affected rows and messages.
- Cap by rows and bytes sets `truncated`; paging returns the continuation; a running query
  returns `running` plus `query_id`; cancellation and close are idempotent.
- Result TTL expires; session close closes results; memory budget is respected.
- Value mapping for null, bool, int, float, NaN/infinity, decimal, temporal, UUID, binary,
  JSON, XML, geometry and arrays.
- Read-only enforcement for SQL, MongoDB, Elasticsearch and Redis; write paths and
  profile confirmations.
- Redis blocking commands and `MULTI` return `unsupported`; family result shapes are
  correct.
- `browse` works across families and reports cache state; `describe_object` returns table
  details for relational and `unsupported` elsewhere; `search_objects` respects exposure
  and limits.

**Editor and snippets**

- List only SQL tabs; get text, revision, caret and selection; edits apply as one undo
  step; visible highlight and dirty state.
- Revision conflict makes no change and returns the current revision.
- Replace and multi-range edits produce the expected text and undo behaviour.
- Run script, statement index and selection; busy tab returns `busy`; unexposed tab returns
  `not_exposed`; read-only violation is rejected; results appear in the tab and History.
- Snippet create/update/move/read; delete approval; global and connection scope; folder
  strings; metadata does not include SQL in the list.

**Terminal**

- Fake backend: screen parsing for plain text, ANSI 16/256/truecolour, bold/underline/
  reverse, cursor, alternate screen, wide and combining characters.
- Input encoding: text, arrows, Home/End, Insert/Delete, Page Up/Down, F1–F12, Ctrl/Alt
  combinations, paste and IME text.
- Resize updates parser and PTY dimensions.
- Scrollback, wheel scrolling, selection, copy and bracketed paste.
- Shell resolution: automatic login shell, custom command, Windows fallback order, cwd and
  environment.
- Lazy start, hidden keeps running, exit status and Restart, Kill terminates the process
  group, app quit kills the process.
- Real-PTY smoke test runs a short command and observes its output/exit.
- No terminal output or shell command appears in logs or diagnostics.

**UI**

- Settings → Agents: enable/disable, port validation, status transitions, copy config,
  rotate token, error states.
- Settings → Terminal: fields, validation, “applies on restart” guidance, open terminal.
- Connection dialog: exposure control defaults and help text; saving persists; duplicate
  resets; explorer badge appears and updates.
- Approval dialog: content, risk label, default focus, Escape denies, countdown, allow and
  deny paths.
- Status bar: hidden when MCP is off; shows status and session count when on; click opens
  settings.
- Terminal panel: toggle in toolbar, View menu, palette and `Ctrl+Backtick`; default
  hidden; opening starts the shell; hiding keeps it alive; height and resize; exit and
  error states; Copy MCP config enabled only when MCP is on; theme/font.
- Focus: typing in the terminal does not trigger editor or global query shortcuts; copying
  and pasting use the correct platform shortcuts; `Ctrl+C` is an interrupt.
- Toasts for agent configuration and snippet writes; no secret content.
- Layout: terminal and history coexist; value panel unaffected; small window minimums.

**Quality gates**

- Unit and integration tests run with `make test-fast`; no new container requirement.
  Container-based driver tests remain optional and gated as today.
- `cargo nextest run --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo fmt` pass.
- The existing egui version and OpenSSL checks still pass.
- Performance budgets are measured: cold start unaffected by a disabled or unopened
  feature; terminal memory bounded by scrollback; binary size within the product target.
- Documentation builds and the guide-completeness test passes.

## Acceptance Criteria

1. MCP is disabled by default; enabling it from Settings starts a loopback-only Streamable
   HTTP server at the configured stable port, and disabling or quitting stops it and
   removes the discovery file.
2. The endpoint requires a persistent keychain-backed bearer token; the token is never in
   TOML, the discovery file, logs, diagnostics or tool output; rotation invalidates old
   tokens and drops sessions.
3. Origin and Host validation rejects non-loopback requests; the discovery file is `0600`
   on Unix and user-scoped on Windows; a busy port produces a clear status and no stale
   file.
4. Every connection defaults to `None`; `Read` and `Write` are human-only; agent-created
   and duplicated connections are `None`; exposed connections are visibly marked.
5. `Read` forces read-only execution for SQL, MongoDB, Elasticsearch and Redis; `Write`
   still obeys profile read-only, commit mode, write confirmations and destructive
   confirmations.
6. Agent connections open lazily and visibly; there is no connect/disconnect tool; agent
   sessions are isolated from editor and other MCP sessions; closing an MCP session rolls
   back its transactions and closes its results.
7. Destructive statements and configuration/snippet deletes require the in-app approval;
   the dialog shows client, connection, database and the exact statement; timeout, deny,
   disconnect and quit all deny; there is no bypass and no “remember”.
8. Connection values and secrets are write-only; secrets are never returned; secret
   writes are autonomous and logged by field name only; profiles, settings, keymap and
   tasks are readable.
9. Driver installation, MCP server settings, application updates and `terminal.shell` are
   human-only; `terminal.shell` is not readable by agents; safety flags are tighten-only,
   including profile changes, setting resets and transport-security driver fields.
10. `config_list`, `config_write` and `config_delete` support the documented targets,
    partial patches, protected-key rejection, live application of changes, approval-gated
    deletes and in-use profile rejection.
11. The 19 documented tools exist with the documented request/response shapes; the guide
    is generated from the registry, covers every tool and contains no secrets.
12. `run_query` is family-aware, executes exactly one statement or command, enforces read
    exposure and profile confirmations, returns typed columns/rows or documents/replies,
    and respects the 30-second deadline, 200-row/256-KiB default cap and 1000-row maximum.
13. Results page by id, cancel and close idempotently, expire after five minutes of
    inactivity and are closed with their MCP session and agent session.
14. `browse`, `describe_object` and `search_objects` expose exposed connections only,
    report cache state and return `unsupported` where a family has no relational describe.
15. Editor tools list only SQL query tabs, read text/revision/caret/selection, apply
    replace or range edits as one undo step with revision conflict protection and visible
    dirty/highlight state, and never create, close or save tabs.
16. `run_query_tab` runs the buffer as a script, a statement index or the selection in the
    tab's session, shows results in the tab, records History, rejects a busy tab and
    enforces exposure and confirmations.
17. Snippets are readable and writable, support global and connection scopes and folders,
    and delete only after approval.
18. The integrated terminal is a real PTY with a login shell, configurable shell/cwd/
    scrollback, lazy start, hidden persistence, Kill/Restart/exit status and process-group
    cleanup on quit.
19. The terminal renders ANSI 16/256/truecolour, cursor, alternate screen, wide characters
    and resize correctly; supports selection, copy/paste and bracketed paste; does not
    implement mouse reporting or clickable links; uses the app theme and monospace font.
20. The terminal toggle is in the toolbar next to value/history, in View, the palette and
    the keymap; its default is `Ctrl+Backtick` on every platform; the panel is bottom,
    resizable, default closed and session-only in visibility; history can coexist above it.
21. `terminal.shell` is unreadable and unwritable via MCP; `terminal.cwd` and
    `terminal.scrollback` are readable/writable; the terminal is not exposed to MCP; its
    output and shell command are never logged; no MCP token is injected into its
    environment and no agent is launched automatically.
22. The terminal header offers Copy MCP config when the server is running; configuration
    is a one-paste setup for a client; the terminal works when MCP is disabled.
23. Settings contains Agents and Terminal sections with clear status, errors, validation
    and “applies on restart” guidance; the connection dialog and explorer expose agent
    access clearly; the status bar shows MCP only when enabled; approvals and toasts follow
    the app design system.
24. Focus behaviour is correct: typing in the terminal does not trigger editor actions;
    platform copy/paste shortcuts and `Ctrl+C` behave as specified; the approval dialog
    defaults to Deny and is keyboard-accessible.
25. All acceptance tests pass with `make test-fast`, `cargo nextest`, clippy and fmt; no
    container requirement is added; the guide-completeness test passes; documentation is
    updated.
26. Cold start, idle memory and binary size remain within the product budgets; terminal
    memory is bounded by the configured scrollback; a disabled or unopened feature has no
    process or listener.
27. No secret, connection value, SQL text, terminal output or shell command appears in
    logs, diagnostics, toasts or MCP errors; errors are structured and sanitized.
28. macOS, Windows and Linux builds pass; no second egui version and no OpenSSL dependency
    are introduced.

## Out of Scope

- MCP resources, prompts, sampling, elicitation, server-initiated requests and OAuth.
- Remote or non-loopback MCP access, TLS, mutual TLS, reverse proxies and multi-user
  authentication.
- Persistent audit tables, an Agents panel and agent markers in History.
- Reading connection values or secrets through MCP; driver installation/removal, rolling
  back drivers, application updates and MCP server configuration through MCP.
- Running saved tasks, administration panels, data transfer, import, export, dump and
  restore through MCP.
- Creating query tabs, closing tabs, saving editors to files, opening a result in a tab and
  controlling the terminal through MCP.
- Multi-statement headless queries outside an editor tab; `run_query` is single-statement.
  Scripts run through `run_query_tab`.
- Multiple terminal sessions or tabs, split panes, mouse reporting, clickable hyperlinks,
  window-title integration and advanced terminal protocols.
- Automatic MCP environment variables in the terminal, automatic agent launch, and writing
  or modifying third-party agent configuration files.
- Persisting terminal visibility or height across restarts; the visibility is session-only.
- Multiple sqlr instances sharing one MCP endpoint; a second instance reports the port as
  busy and keeps running without MCP.
- Agent access to the terminal; the terminal is human-only.
- Telemetry of any kind.
- A built-in agent chat UI or a built-in MCP client; agents are external clients.

## Further Notes

### Why this shape

The feature deliberately keeps three boundaries:

- **Exposure is consent.** Nothing is reachable by an agent until the person marks a
  connection `Read` or `Write`. Duplicating a connection does not inherit consent, and an
  agent cannot grant it to itself.
- **Credentials are write-only.** Agents can complete a connection but cannot read what
  they wrote or any existing value. This keeps infrastructure and passwords out of the
  model context while still allowing setup automation.
- **The terminal is human.** It is the place where the person runs an agent, not a tool the
  agent can drive. That is why `terminal.shell` is not even readable: a writable shell
  command would turn “open the terminal” into arbitrary code execution chosen by the
  agent.

The no-persistent-audit decision is a trade-off. The rotating log records agent tool
calls, but it is not a queryable audit trail. The usage guide must say so, and the token
rotation action is the primary containment control. Query History remains the place to
inspect what ran; it is not an attribution system.

### UX principles

- Use the existing theme tokens, spacing scale, typography and icons; no new palette.
- Prefer inline status and clear empty/error states over new panels or modal flows.
- Keep the terminal and history unobtrusive: they are bottom panels, not dock tabs, and
  they preserve the central editor layout.
- Make the safe choice the default in approvals and make the exact consequences visible.
- Use consistent vocabulary: “agent access”, “exposed”, “approval”, “MCP session”,
  “terminal”. Do not call the MCP server a “remote” or the terminal an “agent console”.

### Risks and mitigations

- `rmcp` API churn: pin an exact version and keep the protocol boundary isolated in
  `sqlr-mcp`.
- Binary-size growth from `rmcp`/`axum`/`portable-pty`/`vt100`: measure against the product
  budget and keep dependency features minimal.
- MCP client differences in HTTP header support and long tool calls: document the setup,
  keep the 30-second deadline and return `running` with a query id rather than hanging.
- Port conflicts and multiple instances: explicit status, no crash, no stale discovery.
- Terminal emulation edge cases: use `vt100`, test wide characters and alternate screen,
  and keep mouse reporting/hyperlinks out of the first version.
- PTY tests are platform-sensitive: keep one smoke test and put the behavioural surface
  behind a fake backend.
- Windows ConPTY and shell detection: test the resolution logic and provide a clear error
  and Retry state.
- Keychain unavailability: fail the server with a precise status; never fall back to a
  plaintext token.
- Approval blocking: the dialog is visible, timed and cancellable; the UI remains
  responsive because approvals are queued and timeouts are asynchronous.

### Documentation notes

The usage guide should include:

- a short “what the agent can and cannot do” table;
- enabling MCP, copying the client configuration and rotating the token;
- the exposure model and how to set it;
- what triggers an approval and what the dialog shows;
- the integrated terminal, Copy MCP config and the fact that the terminal is not an MCP
  tool;
- troubleshooting: server off, port busy, keychain error, wrong token, 403 Origin/Host,
  session cap, timed-out approval, expired result;
- the no-persistent-audit trade-off and where the log lives.

The technical document should include the exact tool schemas, error codes, result union,
value mapping, limits, lifecycle state machine, discovery file shape and the session/agent
session cleanup rules.
