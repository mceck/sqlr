# Agent access: MCP transport, tools and the terminal

This document describes the technical contract behind sqlr's agent access and integrated
terminal. The user-facing guide is [Agents and the terminal](../usage/agents.md).

## Boundaries

| Crate | Responsibility | Depends on |
|---|---|---|
| `sqlr-mcp` | listener, authentication, discovery, MCP sessions, tool registry, policy, guide, `AgentHost` | `sqlr-core`, `sqlr-engine`, `sqlr-store`, `rmcp`, `axum` |
| `sqlr-terminal` | PTY session, vt100 parser, input encoding, screen snapshot, egui widget | `egui`, `portable-pty`, `vt100` |
| `sqlr-app` | `AgentHost` implementation, lifecycle, approvals, Settings, terminal panel, toolbar/menu/keymap | all |

`sqlr-mcp` never depends on `sqlr-ui` or `sqlr-app`; `sqlr-terminal` never depends on
either `sqlr-mcp` or `sqlr-app`.

## Transport lifecycle

- MCP Streamable HTTP at `POST /mcp`; `rmcp` is pinned to `=3.7.0`, `axum` 0.8, no TLS.
- The listener binds `127.0.0.1:<port>` (default 47821). There is no wildcard, IPv6
  wildcard or Unix socket option.
- Disabled by default. Starting reads/generates the keychain token, binds the listener,
  builds the session manager and router, publishes `mcp.json`, then reports Running.
- Stop cancels in-flight calls, denies approvals, drops sessions (and their agent
  sessions/results), removes the discovery file, then reports Stopped. Application quit
  does the same.
- Port changes restart the listener; token rotation replaces the keychain value and drops
  all sessions while staying bound.
- At most eight sessions. The manager rejects the ninth. An idle sweep closes sessions
  with no in-flight call for 30 minutes.
- `mcp.json` contains `url`, `pid`, `protocol_version`, `app_version` and `started_at`,
  never the token. It is written atomically (temp + rename), `0600` on Unix, and removed
  on stop.

### Local security

- Every request needs `Authorization: Bearer <token>` (constant-time compare), including
  initialize. Missing/wrong is `401` with `WWW-Authenticate: Bearer`.
- Host must be `127.0.0.1:<port>` or `localhost:<port>`; Origin, when present, must be
  `http://127.0.0.1:<port>` or `http://localhost:<port>`. `null`, remote and arbitrary
  values get `403`. No CORS preflight is served.
- Only MCP tools are exposed: no resources, prompts, sampling, elicitation or
  server-initiated requests.
- The initialize response instructs the agent to call `get_guide` and explains consent.

## `AgentHost`

Queries, catalog reads and snippets execute against the engine on its tokio runtime.
Everything that needs the UI thread goes through `AgentHost`:

```rust
async fn connection(&self, id: &str) -> ToolResult<AgentConnection>;
async fn config_write / folder / delete;
async fn settings / keymap / tasks / profiles / drivers / writes;
async fn approve(&self, request: ApprovalRequest) -> ToolResult<()>;
async fn tabs / tab / edit_tab / run_tab;
async fn toast(&self, text: &str);
```

The application implements it with a request channel and one-shot replies. A request is
enqueued, the UI is woken, and it is processed at the start of the next frame. The UI
thread never performs I/O or waits for a database. Connection writes go through the
existing store worker; the MCP call is acknowledged only after the store replies.

## Exposure and sessions

`AgentAccess` is `None | Read | Write` (serde default `None`) on `ConnectionConfig`.

- No exposure: the connection is absent from agent lists and every data tool returns
  `not_exposed`.
- Read: the engine agent session is prepared read-only and the family classifier rejects
  modifying statements before execution.
- Write: the profile still decides commit mode, read-only, write confirmation and
  destructive confirmation.

The engine has a third slot, `Slot::Agent(session_id)`, beside `Slot::Meta` and
`Slot::Tab`. The agent session is keyed by MCP session and connection, so it never shares
a transaction with an editor tab or another MCP session. Connections open lazily on the
first catalog read or query; the metadata session opens so the explorer shows the
connection. There is no connect/disconnect tool. On MCP session end, idle timeout, token
rotation, server stop or quit: in-flight queries are cancelled, results closed, agent
sessions closed (rolling back open transactions) and a metadata session opened only for
that agent is closed. A connection the person already had open stays open.

## Tools

There are 19 tools, generated from one registry (the guide is generated from the same
registry): `get_guide`, `config_list`, `config_write`, `config_delete`, `browse`,
`describe_object`, `search_objects`, `run_query`, `get_query_result`, `cancel_query`,
`close_result`, `list_query_tabs`, `get_query_tab`, `edit_query_tab`, `run_query_tab`,
`list_snippets`, `get_snippet`, `write_snippet`, `delete_snippet`.

### Query contract

`run_query` executes exactly one SQL statement, one document shell statement, one
Elasticsearch console request, or one Redis command line. SQL with more than one statement
returns `invalid` with the count and a hint to use a tab. Redis rejects `MULTI`, `EXEC`,
`DISCARD`, `MONITOR`, `SUBSCRIBE`, `PSUBSCRIBE` and blocking commands with `unsupported`.

- Deadline: 30 s default, 1–120 s via `timeout_s`.
- Page: 200 rows default, 1–1000 via `max_rows`, at most 256 KiB encoded rows. Either cap
  sets `truncated` and leaves the `query_id` pageable.
- Results expire five minutes after the last access; a background sweep closes them.
- The result is a tagged union:

```json
{
  "kind": "tabular | document | reply",
  "status": "ready | running | cancelled | failed",
  "query_id": "…",
  "elapsed_ms": 0,
  "truncated": false,
  "error": {"code": "…", "message": "…"},
  "tabular": {"columns": [], "rows": [], "affected": null, "messages": []},
  "document": {"documents": []},
  "reply": {"value": null}
}
```

Only the member matching `kind` is present. Value mapping: `NULL` → JSON null; booleans
and integers → JSON booleans/numbers; finite floats → numbers and `NaN`/`Infinity`/
`-Infinity` → strings; exact decimals, temporal values, intervals, UUIDs, XML, geometry
and arrays → their grid text; binary → base64; JSON columns → parsed JSON when valid.
Columns carry `name`, `type_name`, `kind` and `hint`.

### Editor contract

Only SQL query tabs are listed. `get_query_tab` returns text, revision, dirty flag, caret
and selection character offsets (end exclusive). `edit_query_tab` takes `expected_revision`
and either `replace` or `edits: [{start, end, text}]`; a mismatch returns `conflict` with
the current revision and applies nothing. An edit is one undo group, marks the tab dirty,
scrolls to and highlights the first change. The agent cannot create, close or save tabs.
`run_query_tab` runs `script`, `statement` with a zero-based index, or `selection` in the
tab's session, rejects a busy tab, enforces exposure/read-only and the profile
confirmations, records History and places results in the tab.

### Configuration contract

- Connection metadata only: id, name, driver, folder, profile, exposure and a
  `secret_fields` map of key → stored. Values and secrets are never returned.
- `config_write` patches are partial; omitted fields stay. `remove_values` and
  `remove_secrets` name keys to delete. `driver` is immutable and `agent_access` is
  rejected.
- Profiles, settings, keymap and tasks are readable and writable (tasks are never run).
- Driver installs, `agents.*`, application updates and `terminal.shell` are human-only.
  `terminal.shell` is not even readable.
- Safety flags are tighten-only at profile, setting and connection level. A connection
  may only move to a profile at least as strict; resetting a tightened setting is
  rejected; transport-security driver fields can only be tightened.
- Deletes (connection, folder, profile, task) and snippet deletes require approval.
  A profile in use is rejected. A connection delete disconnects and removes secrets; a
  folder delete moves its contents to the parent.

## Errors

Tool failures are MCP tool errors (`isError: true`) with both readable text and
`structuredContent: {"error":{"code","message"}}`. Codes: `invalid`, `not_exposed`,
`read_only`, `secret_required`, `denied`, `timeout`, `not_found`, `conflict`, `busy`,
`unsupported`, `approval_required`, `permission`, `connection`, `catalog`, `error`,
`internal`. Messages never include stack traces, secrets or connection values.

## Terminal

`TerminalSession` owns the PTY master, a reader thread feeding a `vt100::Parser`, a
monitor thread waiting for the child, and a writer. The UI locks the parser only long
enough to copy a screen snapshot. A configured command is run through the platform shell
(`$SHELL -lc` on Unix, `cmd /C` on Windows); the default is `$SHELL -l` or
PowerShell/`cmd.exe`. `TERM=xterm-256color` and `COLORTERM=truecolor` are set; MCP tokens
are not injected. Settings are global and apply on Restart. Hiding the panel does not kill
the child; quitting the app kills the process group. Output and the shell command are
never logged.
