---
status: done
---

# MCP agent access and the integrated terminal

## Specs

- [Agents and terminal](../specs/agents-and-terminal.md)
- [Connections](../specs/connections.md)
- [Query execution](../specs/query-execution.md)
- [Query editor](../specs/query-editor.md)
- [Results and editing](../specs/results-and-editing.md)
- [Drivers](../specs/drivers.md)
- [Explorer and objects](../specs/explorer-and-objects.md)
- [Settings and workspace](../specs/settings-and-workspace.md)
- [Diagnostics](../specs/diagnostics.md)

## Problem Statement

People who use sqlr together with coding agents bridge the two worlds by hand. The Agent cannot see the Connections, schemas, open queries or Snippets sqlr has already loaded, so SQL and schema fragments are copied back and forth and query results are pasted into the chat. Configuration changes mean leaving the Agent, editing a form and returning. There is also nowhere to run the Agent without leaving sqlr: a separate terminal window loses the link to the running application and needs the MCP endpoint configured again.

The feature has to solve this without turning the app into a remote-control surface for any process on the machine. Agents act with the person's rights, can read data and can write configuration, so exposure must be explicit, credentials must stay out of reach, the existing transaction and confirmation rules must hold, and the app log must record what Agents did.

## Solution

sqlr exposes a native MCP server while it runs. It listens only on the loopback interface, is off by default and is configured in Settings → Agents. A persistent bearer token lives in the shared encrypted Credential vault behind the OS keychain, and a permission-protected discovery file publishes the endpoint without the token. The server speaks MCP Streamable HTTP with a typed tool surface: a guide, catalog browsing, queries, editor and Snippet operations, and configuration management with write-only connection values.

Each Connection has an Agent access level, `None`, `Read` or `Write`, chosen by the person and `None` by default. `Read` forces a read-only engine Session; `Write` still follows the Connection profile. Destructive statements and configuration and Snippet deletes go through the same in-app Approval dialog the person already sees, with a deny-by-default timeout. Driver installs, MCP settings, application updates and the terminal shell command stay human-only.

The app also gains an Integrated terminal at the bottom of the workspace: a real PTY with a login shell, toggled with `Ctrl+Backtick`, kept running when hidden, where the person can launch Claude Code, Codex or any other Agent. It is human-only, never exposed to MCP, and its output and shell command are never logged.

## User Stories

1. As a developer, I want sqlr to expose an MCP server only while the app is open, so that no background daemon keeps running after I quit.
2. As a security-conscious user, I want MCP off by default, so that no local endpoint exists until I enable it.
3. As a user, I want a stable loopback URL and a persistent bearer token, so that I configure my Agent once instead of after every restart.
4. As a user, I want the token stored in the shared encrypted Credential vault with its key protected by the OS keychain and rotatable from Settings, so that I can revoke access without editing files.
5. As a user, I want the endpoint, port, status and active session count in Settings, so that I can diagnose why an Agent cannot connect.
6. As a user, I want a discovery file with the URL, process id and protocol version but no token, so that local tooling can find the endpoint without exposing the secret.
7. As a user, I want the server to bind only to `127.0.0.1` and reject non-loopback `Origin` and `Host` headers, so that websites and other machines cannot reach it.
8. As a user, I want a clear error when the configured port is already in use, so that a second sqlr instance does not crash or silently take over.
9. As a user, I want the server to stop and remove its discovery file when I disable it or quit, so that stale endpoints do not linger.
10. As a user, I want a session cap and an idle timeout, so that a forgotten client cannot hold resources forever.
11. As a user, I want to copy a complete MCP client configuration from Settings, so that connecting an Agent is one paste.
12. As a user, I want to see the connected client name and version in Approvals and logs, so that I know which Agent is acting.
13. As a user, I want to rotate the token and drop active sessions, so that a leaked token can be revoked immediately.
14. As a user, I want MCP tools only, without resources or prompts, so that the surface stays predictable and uniformly protected.
15. As an Agent, I want a guide tool that explains the application and the other tools, so that I can onboard myself without guessing.
16. As a user, I want every Connection to default to not exposed, so that no database is reachable by Agents without my consent.
17. As a user, I want to choose None, Read or Write per Connection, so that I can allow inspection without allowing modification.
18. As a user, I want the exposure control in the connection dialog, so that it is set where the Connection itself is defined.
19. As a user, I want exposure to be human-only, so that an Agent cannot expose a Connection it created.
20. As a user, I want a duplicated Connection to reset to not exposed, so that copies do not silently inherit Agent access.
21. As an Agent, I want to see only the metadata of exposed Connections, so that I can choose a target without reading infrastructure values.
22. As an Agent, I want to browse schemas, search objects and describe tables, so that I can write correct queries.
23. As an Agent, I want read exposure to force read-only execution, so that a mistake cannot modify data.
24. As an Agent, I want write exposure to respect the Connection profile, so that production confirmations still apply.
25. As a user, I want the Agent's Connections to open lazily and visibly, so that I do not manage connect or disconnect on its behalf.
26. As a user, I want an Agent not to disconnect my Connections, so that its work cannot interrupt mine.
27. As a user, I want an Agent's Sessions isolated from my editor's transactions, so that its `BEGIN` and `COMMIT` cannot affect my work.
28. As a user, I want the Agent's open transactions rolled back when its MCP session ends, so that nothing is left uncommitted.
29. As an Agent, I want a `secret_required` error when credentials are missing, so that I can ask for or write them.
30. As an Agent, I want to query SQL, MongoDB, Elasticsearch and Redis through one family-aware tool, so that I do not need different APIs.
31. As a user, I want read exposure enforced for every family, so that MongoDB, Elasticsearch and Redis writes are also blocked.
32. As a user, I want Agent queries in Operations and History, so that I can see what ran and watch long operations.
33. As a user, I want destructive Agent statements to open the same confirmation dialog as my own statements, so that no Agent has a bypass.
34. As a user, I want the Approval to show client, Connection, database and the exact statement, so that I can decide safely.
35. As a user, I want Approvals to time out and deny, so that a stuck Agent does not wait forever.
36. As a user, I want configuration deletes and Snippet deletes to require Approval, so that an Agent cannot silently destroy my work.
37. As a user, I want safety flags in profiles and settings to be tighten-only for Agents, so that an Agent cannot disable its own guardrails.
38. As a user, I want driver installs, MCP server settings, application updates and the terminal shell command to be human-only, so that an Agent cannot gain code execution or open a remote endpoint.
39. As a user, I want secret writes to be autonomous but logged by field name only, so that Connection setup is fast without exposing values.
40. As a user, I want secrets never returned by any tool, so that credentials never enter the model context.
41. As a user, I want Connection values write-only, so that hosts, users, options and other infrastructure details are not disclosed.
42. As a user, I want profiles, settings, keymap and task definitions readable, so that Agents can avoid breaking policy.
43. As a user, I want a structured `denied` or `timeout` error when I refuse or miss an Approval, so that the Agent can explain what happened.
44. As an Agent, I want a synchronous query result with a row and byte cap, so that one call is useful without flooding my context.
45. As an Agent, I want to page a Result by id, so that I can inspect more without rerunning the query.
46. As an Agent, I want to cancel a query, so that a long scan does not block me.
47. As an Agent, I want Results to expire, so that forgotten Results do not exhaust the application's memory.
48. As an Agent, I want typed columns and arrays of rows, so that duplicate column names are unambiguous.
49. As an Agent, I want exact decimals, timestamps and UUIDs as strings, so that precision is preserved.
50. As an Agent, I want binary values in base64 and JSON columns parsed, so that I can handle every value class.
51. As an Agent, I want to list the open SQL Editor tabs, so that I can choose one to fix.
52. As an Agent, I want to read a tab's text and revision, so that I can propose a correction.
53. As an Agent, I want to replace the whole text or apply precise edits with a revision check, so that I do not overwrite what the person is typing.
54. As a user, I want Agent edits to be one undo step, visibly highlighted and marked dirty, so that I can review and revert them.
55. As an Agent, I want to run the tab's whole buffer as a script, a single statement or the current selection, so that I can validate the fix in context.
56. As a user, I want a busy tab to reject an Agent's run, so that my running query is not silently cancelled.
57. As an Agent, I want to read, create, update, move and delete Snippets, so that I can save reusable SQL.
58. As a user, I want Snippet deletion approved, so that an Agent cannot lose my library.
59. As an Agent, I want to create and update Connections, Folders, profiles, settings and keymap, so that I can automate setup.
60. As an Agent, I want to delete Connections, Folders, profiles and tasks only with Approval, so that destructive configuration changes are explicit.
61. As an Agent, I want Driver manifests, so that I can create Connections with the right fields.
62. As an Agent, I want to know whether a secret exists without reading its value, so that I can complete a Connection.
63. As an Agent, I want settings changes applied to the running application, so that they take effect immediately.
64. As a user, I want a profile that is in use not to be deleted, so that Connections do not break.
65. As a user, I want an Integrated terminal at the bottom that opens with a toggle, so that I can run an Agent without leaving sqlr.
66. As a user, I want the terminal toggle next to the value-panel and history toggles, so that all workspace panels are in one place.
67. As a user, I want `Ctrl+Backtick` on every platform, so that the terminal is one keystroke away.
68. As a user, I want a real PTY with a login shell, so that Claude Code, Codex and my tools on `PATH` work normally.
69. As a user, I want the shell command, working directory and scrollback configurable, so that the terminal fits my environment.
70. As a user, I want the terminal to keep running when hidden, so that a long Agent task is not interrupted.
71. As a user, I want Restart, Close and the exit code, so that I can recover from a dead shell and dismiss the panel from where I work.
72. As a user, I want ANSI colours, cursor, alternate screen, wide characters and resize, so that TUI Agents render correctly.
73. As a user, I want selection, copy, paste and bracketed paste, so that I can work efficiently and paste multi-line prompts safely.
74. As a user, I want the terminal to use my theme and monospace font, so that it feels native to sqlr.
75. As a user, I want a Copy MCP config action in Settings → Agents, so that I can configure the Agent with one paste without crowding the terminal header.
76. As a user, I want no automatic token environment variables and no automatic Agent launch, so that secrets do not leak into the PTY and I stay in control.
77. As a user, I want `terminal.shell` unreadable and unwritable by Agents, so that an Agent cannot make my terminal execute arbitrary code.
78. As a user, I want `terminal.cwd`, `terminal.font_size` and `terminal.scrollback` readable and writable by Agents, so that harmless preferences can be automated.
79. As a user, I want terminal output not logged and the process group killed on quit, so that secrets do not leak and Agents do not linger.
80. As a user, I want the terminal to work when MCP is off, so that it is a normal terminal first.
81. As a user, I want terminal settings in Settings with clear "applies on restart" guidance, so that I know when changes take effect.
82. As a user, I want the current Agent access level checked in the Connection menu, so that I can review what Agents can reach.
83. As a user, I want a discreet MCP status indicator in the status bar, so that I know when remote control is active.
84. As a user, I want a well-designed Approval dialog with risk labels, a monospace statement and obvious keyboard actions, so that decisions are fast and safe.
85. As a user, I want toasts for Agent configuration and Snippet changes, so that I notice side effects without opening a log.
86. As a user, I want clear empty, loading, running, exited and error states in Settings and the terminal, so that I always know what to do next.
87. As a user, I want terminal keystrokes not to trigger editor shortcuts, so that typing into an Agent is reliable.
88. As a user, I want structured tool errors without stack traces, so that Agent failures are actionable.
89. As a user, I want the guide to include recipes, so that Agents use the tools in the intended order.
90. As a user, I want the terminal, history and value panels to coexist, so that I can arrange my workspace.
91. As a user, I want the terminal resizable with a sensible minimum, so that it fits small windows.
92. As a user, I want no shell to start until I open the terminal, so that cold start and idle memory are unaffected.
93. As a user, I want the feature to work on macOS, Windows and Linux, so that Agent access is not platform-specific.
94. As an Agent, I want to know whether a Connection is exposed to me, so that I can explain a refusal.
95. As an Agent, I want a guide that lists the error codes and the next step for each, so that I recover without guessing.

## Implementation Decisions

- A dedicated MCP component owns the listener, authentication, discovery, MCP sessions, tool registry, policy, Approval requests and the guide. It depends on the core model, the engine and the store, and never on the UI or the application.
- A dedicated terminal component owns the PTY session, parser, input encoding, screen snapshot and terminal widget. It depends on egui and the PTY library only. It never depends on the MCP component or the application.
- The application implements the host interface the MCP component calls for anything that needs the UI thread: Connection metadata, configuration stores, editor tabs, Approvals, toasts and the client configuration. Requests travel through a channel with one-shot replies, the UI is woken by a repaint and processes them at the start of the next frame.
- Queries, catalog reads and Snippet operations run against the engine on its runtime. Configuration writes use the existing store worker and in-memory stores and are acknowledged only after the save confirms.
- The core model gains an Agent access enum with `None` as the default, serialized with a default on the Connection.
- The engine gains a third Session slot keyed by MCP session and Connection, with preparation for read-only Sessions, lazy opening, cleanup of every Agent Session of one MCP session, and cleanup of every Agent Session of one Connection.
- The store keeps the MCP token in the shared Credential vault under its own namespace. Duplicating a Connection resets Agent access; imports never grant it.
- The server transport is MCP Streamable HTTP through a pinned protocol library and a minimal HTTP stack, with no TLS features and no OpenSSL. The listener binds only loopback.
- Authentication is a middleware accepting only the configured loopback `Host` and `Origin`, then comparing the bearer token in constant time. Missing or wrong tokens answer `401` with `WWW-Authenticate: Bearer`; refused hosts and origins answer `403`.
- The session manager caps MCP sessions at eight, registers client name and version, tracks in-flight calls and recent activity, and runs a sweep for idle sessions and expired Results every 60 seconds.
- Discovery is a JSON file written atomically with owner-only permissions, containing no token, removed on stop and overwritten when stale.
- The tool registry is the single source of truth for tool names, descriptions, input schemas and guide argument summaries. Tools are flat and nineteen in number.
- Policy lives in one module: SQL splitting and classification, Approval decisions from the profile, profile strictness comparison, setting value parsing, manifest rendering without values, transport-security field detection, and error sanitization.
- Result retention is an in-memory store keyed by an opaque id, with an owner MCP session, a Result kind, a last-access time and a five-minute TTL. Pages cap rows and encoded bytes and fetch more from the engine when the offset passes the loaded rows.
- The query contract is family-aware: one statement for SQL, one shell statement for MongoDB, one console request for Elasticsearch, one command line for Redis, with Redis multi/transaction, monitoring, subscription and blocking commands rejected as unsupported.
- Editor tools use an atomic editor API that replaces all text or applies a validated range list as one undo group with revision checks and caret adjustment. The existing editor stays the owner of text, carets, markers and undo history.
- Snippets persist through the existing library and produce a toast. Configuration deletes always request an Approval before invoking the host.
- The terminal session owns the PTY master, reader thread, writer, monitor thread, parser and resize handle. The UI reads snapshots and sends input, resize and control messages.
- Shell resolution uses the platform login shell by default and the platform shell wrapper for a configured command. The shell environment sets `TERM` and `COLORTERM` and carries no MCP token.
- The automatic terminal workspace is prepared on a background worker: an owner-only folder, `AGENTS.md` created only when missing, `CLAUDE.md` linked to it when absent, and `.mcp.json` written atomically and refreshed on every server change.
- The terminal panel owns visibility, focus, selection, scrollback position, the session and the settings for the next Restart. Visibility and height stay in session state; terminal settings are ordinary global settings.
- The application adds Settings sections for Agents and Terminal, the exposure control in the connection dialog, the Explorer Agent access submenu, the Approval dialog, the MCP status segment, the toolbar toggle and the terminal panel. The terminal toggle is a global action with `Ctrl+Backtick` as its default, available in the toolbar, View menu, palette, shortcuts dialog and keymap.

## Testing Decisions

- **MCP protocol boundary**: the highest seam. Start the real server on an ephemeral loopback port with a real engine over SQLite and the fake driver, and a scripted host for UI operations. Drive it with an in-process MCP HTTP client through initialize, tool listing and tool calls. It covers authentication, Host and Origin validation, discovery, session cap, exposure, read-only enforcement, Approvals, caps, paging, TTL, cancellation, configuration rules, editor revisions and the guide. Prior art: the engine integration tests that build an engine with the fake driver.
- **Terminal facade**: exercise the real PTY session and widget through a fake PTY backend for screen parsing, input encoding, resize, scrollback, selection, copy, paste, bracketed paste, mouse and kitty input, exit and restart, plus one real-PTY smoke test on a platform command. Prior art: the terminal crate's fake backend and real PTY tests.
- **Engine tests**: Agent Sessions, transaction isolation between two MCP sessions and an Editor session, rollback on session close, read-only enforcement, headless document and key-value queries, Result caps and cancellation. Prior art: the existing Session, transaction and Result tests.
- **Store tests**: Agent access serialization and backward compatibility, duplicate reset, import reset, settings validation and keymap writes, and token storage against the in-memory secret store. Prior art: the Connection repository and vault suites.
- **Application UI tests** with the egui_kittest harness: Settings → Agents and Terminal, the connection dialog exposure control, Explorer menu, the Approval dialog's content, default focus, Escape, timeout and allow and deny paths, the status bar segment, the terminal panel toggle, focus and lifecycle, and toasts. Prior art: the existing application harness with the fake driver and memory secret store.
- **Guide completeness test**: every registered tool appears in the guide, and the guide contains no secret-like text.

## Acceptance Criteria

- [x] MCP is off by default; enabling it starts a loopback-only Streamable HTTP server on the configured port, and disabling or quitting stops it and removes the discovery file.
- [x] The endpoint requires a persistent keychain-backed bearer token; the token stays out of TOML, the discovery file, logs, diagnostics and tool output; rotation invalidates old tokens and drops sessions.
- [x] Origin and Host validation rejects non-loopback requests; the discovery file is `0600` on Unix and user-scoped on Windows; a busy port produces a clear status and no stale file.
- [x] Every Connection defaults to `None`; `Read` and `Write` are human-only; Agent-created and duplicated Connections are `None`.
- [x] `Read` forces read-only execution for SQL, MongoDB, Elasticsearch and Redis; `Write` still obeys profile read-only, commit mode, write confirmations and destructive confirmations.
- [x] Agent Connections open lazily and visibly; there is no connect or disconnect tool; Agent Sessions are isolated from the editor and other MCP sessions; closing an MCP session rolls back its transactions and closes its Results.
- [x] Destructive statements and configuration and Snippet deletes require the in-app Approval; the dialog shows client, Connection, database and the exact statement; timeout, deny, disconnect and quit all deny; there is no bypass and no remember option.
- [x] Connection values and secrets are write-only; secrets are never returned; secret writes are autonomous and logged by field name only; profiles, settings, keymap and tasks are readable.
- [x] Driver installation, MCP server settings, application updates and `terminal.shell` are human-only; `terminal.shell` is not readable by Agents; safety flags are tighten-only, including profile changes, setting resets and transport-security driver fields.
- [x] `config_list`, `config_write` and `config_delete` support their documented targets, partial patches, protected-key rejection, live application of changes, approval-gated deletes and in-use profile rejection. Saved tasks are readable and deletable through MCP.
- [x] The 19 documented tools exist with the documented request and response shapes; the guide is generated from the registry, covers every tool and contains no secrets.
- [x] `run_query` is family-aware, executes exactly one statement or command, enforces read exposure and profile confirmations, returns typed columns and rows or documents and replies, and respects the 30-second deadline, 200-row and 256 KiB default cap and 1000-row maximum.
- [x] Results page by id, cancel and close idempotently, expire after five minutes of inactivity and are closed with their MCP session and Agent Session.
- [x] `browse`, `describe_object` and `search_objects` expose exposed Connections only, report cache state and return `unsupported` where a family has no relational describe.
- [x] Editor tools list only SQL query tabs, read text, revision, caret and selection, apply replace or range edits as one undo step with revision conflict protection and visible dirty and highlight state, and never create, close or save tabs.
- [x] `run_query_tab` runs the buffer as a script, a statement index or the selection in the tab's Session, shows Results in the tab, records History, rejects a busy tab and enforces exposure and confirmations.
- [x] Snippets are readable and writable, support global and Connection scopes and folders, and delete only after Approval.
- [x] The Integrated terminal is a real PTY with a login shell, configurable shell, directory and scrollback, lazy start, hidden persistence, Restart, Close and exit status, and process-group cleanup on quit.
- [x] The terminal renders ANSI 16, 256 and truecolour, cursor, alternate screen, wide characters and resize correctly; supports selection, copy, paste and bracketed paste; forwards mouse reports to applications that request them, with Shift preserving local selection and scrollback; uses the app theme and monospace font. Clickable links are absent.
- [x] The terminal toggle is in the toolbar next to value and history, in View, the palette and the keymap; its default is `Ctrl+Backtick` on every platform; the panel is bottom, resizable, default closed and session-only in visibility; history can coexist above it.
- [x] `terminal.shell` is unreadable and unwritable through MCP; `terminal.cwd`, `terminal.font_size` and `terminal.scrollback` are readable and writable; the terminal is not exposed to MCP; its output and shell command are never logged; no MCP token enters its environment and no Agent is launched automatically.
- [x] Settings → Agents offers Copy MCP config when the server is running; the configuration is a one-paste setup for a client; the terminal works when MCP is disabled.
- [x] Settings contains Agents and Terminal sections with status, errors, validation and "applies on restart" guidance; the connection dialog and Explorer expose Agent access; the status bar shows MCP only when enabled; Approvals and toasts follow the app design system.
- [x] Focus behaviour is correct: typing in the terminal does not trigger editor actions; platform copy and paste shortcuts and `Ctrl+C` behave as specified; the Approval dialog defaults to Deny and is keyboard-accessible.
- [x] The workspace tests, clippy and fmt pass; no container requirement is added; the guide-completeness test passes.
- [x] Cold start, idle memory and binary size remain within the product budgets; terminal memory is bounded by the configured scrollback; a disabled server and an unopened terminal start no process or listener.
- [x] No secret, Connection value, SQL text, terminal output or shell command appears in logs, diagnostics, toasts or MCP errors; errors are structured and sanitized.
- [x] macOS, Windows and Linux builds pass; no second egui version and no OpenSSL dependency are introduced.

## Out of Scope

- MCP resources, prompts, sampling, elicitation, server-initiated requests and OAuth.
- Remote or non-loopback MCP access, TLS, mutual TLS, reverse proxies and multi-user authentication.
- Persistent audit tables, an Agents panel and Agent markers in History.
- Reading Connection values or secrets through MCP; driver installation and removal, rolling back drivers, application updates and MCP server configuration through MCP.
- Running saved tasks, administration panels, Data transfer, import, export, Dump and Restore through MCP.
- Creating query tabs, closing tabs, saving editors to files, opening a Result in a tab and controlling the terminal through MCP.
- Multi-statement headless queries outside an Editor tab; scripts run through `run_query_tab`.
- Multiple terminal sessions or tabs, split panes, clickable hyperlinks, window-title integration and advanced terminal protocols beyond mouse tracking and the kitty keyboard protocol.
- Automatic MCP environment variables in the terminal, automatic Agent launch, and writing or modifying third-party Agent configuration files.
- Persisting terminal visibility or height across restarts.
- Multiple sqlr instances sharing one MCP endpoint; a second instance reports the port as busy and keeps running without MCP.
- Agent access to the terminal; the terminal is human-only.
- Telemetry of any kind.
- A built-in Agent chat UI or a built-in MCP client.

## Further Notes

This task records retroactively the area as it exists. Agent access is shown in the Connection menu. Saved tasks are readable and deletable through MCP. The terminal forwards mouse reports to applications that request them; Shift keeps selection and scrollback local.
