//! The Markdown guide, generated from the tool registry so it cannot drift.

use crate::registry::TOOLS;

const OVERVIEW: &str = "\
# sqlr and agents

sqlr is a native database client. This MCP server exists only while the application is
running: there is no daemon and no endpoint after it quits. The server listens on
`127.0.0.1` only.

## Required first call

At the start of every new MCP session, call `get_guide` with `{}` and read this overview
before calling any other tool. The default topic is `overview`. This is an agent
workflow requirement; the server does not enforce call order.

## Start here

1. Discover connections: `config_list({\"target\":\"connections\"})`. Use the returned `id`
   as `connection_id`; names and IDs are different. An empty list means no connections
   are exposed: ask the person to expose one in sqlr, rather than guessing an ID.
2. Inspect the catalog: `browse({\"connection_id\":\"<id>\",\"path\":[]})`. Expand nodes by
   passing their returned `path` unchanged. Catalog layouts differ by driver: do not
   invent database or schema segments. Use `has_children` to decide what to expand.
3. For a relational table, call `describe_object` with its exact name and the discovered
   `database` and `schema`, when applicable. Names are case-sensitive according to the
   database. Pass these fields separately, without SQL identifier quotes. Descriptions
   use the catalog session and may not see uncommitted changes in an agent transaction.
4. Run one statement with `run_query`. sqlr connects automatically; there is no separate
   connect tool and no need to run a dummy query before catalog tools.
5. Inspect `status` and `error`, read or page the result, then call `close_result`.

## Choose the right tool

| Goal | Tools |
|---|---|
| Find available connections or inspect configuration | `config_list` |
| Explore catalog children / inspect table structure | `browse` / `describe_object` |
| Find relational objects by name, not row contents | `search_objects` |
| Execute one statement or native command and get data | `run_query` |
| Poll or page its retained result / cancel / release | `get_query_result` / `cancel_query` / `close_result` |
| Read or change SQL already in an editor tab | `list_query_tabs`, `get_query_tab`, `edit_query_tab` |
| Execute an existing tab's script, statement or selection | `run_query_tab` |
| Save reusable SQL without running it | `list_snippets`, `get_snippet`, `write_snippet`, `delete_snippet` |
| Change saved application configuration, not database rows | `config_write`, `config_delete` |

`run_query` returns data directly through MCP. `run_query_tab` starts execution in the
visible editor using its connection and database; its response belongs to the UI flow,
not the retained-result paging API. MCP cannot create, close or save editor tabs.

## Read results correctly

Successful data/configuration tools return JSON in `structuredContent` (also rendered
as text by MCP clients); `get_guide` returns Markdown text. Tool failures set `isError`
and return `{\"error\":{\"code\":\"...\",\"message\":\"...\"}}`. Query execution can also return
`status:\"failed\"` with an `error`: a successful MCP call does not mean SQL succeeded.

For `run_query`, inspect `kind`: `tabular` has `tabular.columns` and row arrays in
`tabular.rows` (values follow column order); `document` has `document.documents`;
`reply` has `reply.value` for a native key-value response. Native commands use the
selected driver's syntax, not SQL. Large integers and decimals can be strings to keep
precision; do not coerce them blindly. A native command error may appear inside
`reply.value.error` even when the result status is `ready`.

- Keep `query_id` from `run_query`. A tab ID and a query ID are different identifiers.
- If `status` is `running` and `query_id` is present, poll `get_query_result` with that
  ID. A key-value command that exceeds the wait deadline currently returns `running`
  without an ID; it cannot be polled/cancelled through these result tools. Report the
  uncertain outcome and ask the person to inspect sqlr. Do not resubmit the command,
  especially a write: that could execute it twice.
- If `truncated` is true, request the next page. Start at offset 0 and advance by the
  number of rows/documents actually returned, not by the requested limit: the byte
  budget may shorten a page. If a running page is empty, keep the same offset.
- Stop on `failed` or `cancelled`; when ready and no longer truncated, paging is done.
  For `reply`, read the value directly; row offsets do not apply.
- `cancel_query` requests cancellation; `close_result` releases retained data. Close
  results when finished. An expired or closed ID cannot be paged again.

## Recover from errors

| Code | Next step |
|---|---|
| `not_exposed` | Ask the person to enable Read/Write access; agents cannot change exposure. |
| `read_only` | Respect the connection/profile restriction; do not retry the write unchanged. |
| `secret_required` | Ask the person to provide missing credentials in sqlr, or write supplied secrets through `config_write`; secrets cannot be read back. |
| `conflict` | Re-read the tab, recompute the edit against its text, then use its new revision. |
| `busy` | The editor tab is already running; inspect it again before another execution. |
| `not_found` | Re-list IDs; for expired results, rerun only if executing again is intended. |
| `denied` / approval `timeout` | Report the refusal/timeout; do not repeatedly reopen approval dialogs. |
| `unsupported` | Check the driver family and database context; `describe_object` and `search_objects` are relational. Omit `database` for single-database drivers such as SQLite. |
| `invalid` | Correct the argument shape using `get_guide` with `topic:\"tools\"`. |
| `catalog` / `connection` / query `timeout` | Report the operation and message; inspect connection state in sqlr. Do not assume changing SQL fixes a connection failure. |

## Exposure is consent

Every connection starts as **not exposed**. The person chooses one of:

- **None** — the connection is invisible to you and every data tool returns `not_exposed`.
- **Read** — you may browse and query it, but only reads; the server session is read-only.
- **Write** — you may write, still under the connection profile's commit mode, read-only
  flag, write confirmation and destructive confirmation.

An agent cannot change exposure. A connection you create is `None`; duplicate a
connection and the copy is `None` too.

## Write-only connections

Connection values (hosts, ports, users, TLS/SSH options and driver properties) and secret
values are **write-only**. `config_list` returns names, drivers, folders, profiles,
exposure and whether each secret field is stored — never a value. `run_query` and catalog
tools work because the application connects on your behalf; they never show you the
address or credentials.

## Approvals

Statements requiring confirmation under the connection profile, and all configuration
or snippet deletes, open an in-app approval dialog. It names you (client name and version), the
connection, database and exact statement, and it times out after 60 seconds with a
`timeout` error. Forbidden data writes are rejected before approval. Secret writes never ask.

## Limits and lifecycle

- `run_query` runs exactly one statement or command. Use an editor tab for scripts.
- The deadline is 30 seconds by default (`timeout_s`, up to 120).
- A page holds at most 200 rows by default (maximum 1000) and 256 KiB of encoded data.
- Results stay available for five minutes after the last access. `close_result` frees one
  immediately; closing the MCP session closes all of them and rolls back open
  transactions.
- At most eight MCP sessions; one is idle after 30 minutes without a call or result use.

## The terminal

sqlr has an integrated terminal, but it is human-only. No MCP tool can read it, write to
it, start it or stop it. The person may run an agent there; that agent is an ordinary MCP
client and is subject to the same exposure, approvals and logging as any other.
";

const SECURITY: &str = "\
# Security model

- The listener binds `127.0.0.1`; `Host` must be `127.0.0.1:<port>` or
  `localhost:<port>`, and an `Origin`, when present, must be the loopback URL. Everything
  else is rejected with 403.
- Every request needs `Authorization: Bearer <token>`, including initialize. The token
  lives in the shared encrypted credential vault, protected by an OS keychain key,
  never in TOML, the discovery file, logs or tool output.
  Rotating it drops every session.
- Only tools are exposed: no resources, prompts, sampling, elicitation or
  server-initiated requests.
- Credentials are write-only. Driver installs, MCP settings, application updates and
  `terminal.shell` are human-only. Safety flags are tighten-only: you may make a profile
  stricter, never looser; you cannot reset a tightened setting; you cannot move a
  connection to a less strict profile; transport-security driver fields can only be
  tightened.
- The rotating application log records tool calls with your client name, the session id,
  target and outcome. It never records SQL text, native commands, result rows, secrets,
  connection values or terminal output. There is **no persistent audit table**: the log
  is the record, and token rotation is the main containment control.
";

const TOOLS_INTRO: &str = "\
# Tool catalogue

Call `get_guide` with `{}` before any other tool in each new session. All tools are
flat and scoped to this sqlr server. The terminal has no tool.

| Tool | Arguments | Purpose and behavior |
|---|---|---|
";

const CONFIG_REFERENCE: &str = r#"
## Argument conventions

Use IDs returned by discovery tools, not display names. `connection_id` is a string;
`tab_id` and `expected_revision` are non-negative integers; `query_id` is the opaque
string returned by `run_query`. Snippets use `id` and the scope field `conn_id`.
Optional fields should be omitted when unused. Database and schema names are separate
fields, not dotted SQL expressions.

`edit_query_tab.edit` accepts `{"replace":"SELECT 1"}` or
`{"edits":[{"start":0,"end":6,"text":"SELECT"}]}`. Ranges use zero-based character
positions, with an exclusive end; use the latest `get_query_tab` text and revision.
`run_query_tab.target` is `"script"`, `"selection"`, or `{"statement":0}` (first statement).

## Configuration payload reference

`config_write` arguments always wrap fields in `{"target":"...","payload":{...}}`.
Read the corresponding `config_list` target first. Singular targets write; plural
targets list. Supported payloads:

| target | payload |
|---|---|
| `connection` | Create: `name`, `driver`, optional `values`, `secrets`, `group`, `profile`, `color`, `save_secrets`. Update: existing `id` plus fields to patch. `driver` cannot change. |
| `folder` | Create: `action:"create", name`. Rename: `action:"rename", from, to`. Move: `action:"move", name, target:{at:"end"}` or `target:{at:"before"/"after"/"into", id:"..."}`. |
| `profile` | Create: `name`. Update: existing `id` plus fields. Optional `color`, `commit_mode` (`auto`/`manual`/`smart`), `confirm_writes`, `confirm_destructive`, `read_only`, `idle_tx_timeout_min`; safety can only tighten. |
| `setting` | `id`, `level:{name:"global"}` or `level:{name:"profile"/"connection",id:"..."}`, and `value` or `reset:true`. Use the listed setting kind, allowed levels and `agent_writable`. |
| `keymap` | `action` from `config_list(target:"keymap")`, plus `chords:["Cmd+Shift+P"]` or `reset:true`. |
| `task` | Create: `name`, `spec`. Update: existing `id` and fields to patch. `spec` is a complete TaskSpec, not SQL text; inspect existing tasks for its structure. |

For connections, driver field keys and string values come from
`config_list({"target":"drivers","driver_id":"<driver>"})`. Put non-secret fields in
`values` and credentials in `secrets`. Omitted update fields are preserved. Connection
values and secrets cannot be read back. `agent_access` is forbidden; the person must
expose new connections in sqlr before you can browse or query them.

`config_delete` takes `target` and `id` at the top level, without `payload`. For a folder,
`id` is its full folder path. Deletes require approval; the dialog explains affected
connections, children or references. Do not assume a configuration delete drops a table.

`write_snippet` replaces the saved snippet: always supply `name` and `sql`. Include the
existing `id`, `folder` and `conn_id` to preserve its identity and scope. Without `id`,
a same-name snippet is updated; omitted `folder` means root and omitted `conn_id` means
unscoped. This differs from connection patch semantics.
"#;

const RECIPES: &str = r#"# Recipes

Start each new session with `get_guide({})`. The examples below show tool names with
JSON arguments, not SQL or JSON-RPC envelopes. Replace placeholders with discovered IDs.

## Discover and describe a relational table

1. `config_list({"target":"connections"})` — choose an exposed connection.
2. `browse({"connection_id":"<id>","path":[]})` — discover root nodes.
3. Call `browse` again with a returned node's full `path` to expand databases/schemas.
4. `describe_object({"connection_id":"<id>","database":"<database>","schema":"<schema>","name":"<table>"})`.
   Omit database/schema if the driver does not use them. Copy the exact table name,
   including case; do not send `schema.table` as `name`. Connections open automatically.
5. Alternatively, `search_objects({"connection_id":"<id>","query":"customer","limit":20})`
   finds table/view names in the default database, not rows. Unreachable connections
   are skipped, so an empty search does not prove an object is absent. Use `browse`
   for other databases or driver families.

## Run SQL and page the result

1. `run_query({"connection_id":"<id>","query":"SELECT 1 AS one","max_rows":100})`.
2. Save its `query_id`, check `status` and `error`, then read `tabular.columns` and
   `tabular.rows` (or the `document` / `reply` field for native commands).
3. If 37 rows were returned and `truncated` is true, call
   `get_query_result({"query_id":"<query_id>","offset":37,"limit":100})`.
   Advance by each actual page count. A short page can still be truncated.
4. Poll a `running` result when it has a `query_id`; do not execute it again to wait
   for completion. If a native key-value command returns no ID, report the uncertain
   outcome for inspection in sqlr instead of retrying it.
5. `close_result({"query_id":"<query_id>"})` when finished, or
   `cancel_query({"query_id":"<query_id>"})` if it should stop first.

`max_rows` controls page size, not the total rows a database query may process. Use
SQL filters / LIMIT or the driver's native equivalent to bound database work.

## Fix an open tab

1. `list_query_tabs({})` — choose a numeric tab ID.
2. `get_query_tab({"tab_id":1})` — inspect text, revision, connection and selection.
3. If revision is 7, `edit_query_tab({"tab_id":1,"expected_revision":7,"edit":{"replace":"SELECT 1"}})`.
   On `conflict`, re-read and recompute; do not overwrite with a stale edit.
4. `run_query_tab({"tab_id":1,"target":{"statement":0}})` runs its first statement.
   Use `"script"` for all statements or `"selection"` for the current selection.
5. The person sees the edit highlighted and can undo it in one step. Use `run_query`
   when you need data returned directly through MCP rather than execution in the editor.

## Save a snippet

`write_snippet({"name":"Example","sql":"SELECT 1","folder":"reports/examples"})` saves
SQL without executing it. To update, read with `get_snippet` and supply its existing
`id`, `folder` and `conn_id` along with the full name and SQL. Deletes require approval.

## Create a connection

1. `config_list({"target":"drivers"})` — choose an installed driver and read its fields.
2. `config_write({"target":"connection","payload":{"name":"Example","driver":"<driver>","values":{"<field>":"<value>"}}})`.
3. Supplied credentials go in `payload.secrets`, a map of secret field keys to strings;
   never put secret fields in `values`. They are accepted but never returned.
4. Ask the person to expose the new connection as Read or Write in sqlr. New connections
   stay unexposed until they do so; creating one does not grant data access.

## Set up an agent in the terminal

Open the terminal (`Ctrl+Backtick`), use **Copy MCP config** in its header, and paste the
configuration into Claude Code, Codex or another client. No token is injected into the
shell environment and no agent is started automatically. MCP cannot control the terminal.
"#;

pub fn guide(topic: Option<&str>) -> String {
    match topic.unwrap_or("overview") {
        "security" => SECURITY.to_string(),
        "tools" => {
            let mut out = TOOLS_INTRO.to_string();
            for tool in TOOLS.iter() {
                out.push_str(&format!(
                    "| `{}` | {} | {} |\n",
                    tool.name, tool.arguments, tool.description
                ));
            }
            out.push('\n');
            out.push_str(CONFIG_REFERENCE);
            out
        }
        "recipes" => RECIPES.to_string(),
        "terminal" => {
            let mut out = String::from(
                "# The integrated terminal\n\n\
The terminal is a real PTY with a login shell, opened with `Ctrl+Backtick` on every\n\
platform and resized at the bottom of the workspace. It keeps running when hidden.\n\
\n\
It is human-only: no MCP tool can read, write, start or stop it. `terminal.shell` is not\n\
readable through MCP; `terminal.cwd`, `terminal.font_size` and `terminal.scrollback` are\n\
ordinary settings.\n\
Output and the shell command are never logged, and no MCP token is placed in its\n\
environment.\n\n\
**Copy MCP config** in the terminal header copies the client configuration (including\n\
the token) for a one-paste setup. The terminal itself works when MCP is disabled.\n",
            );
            out.push_str("\n## Related tools\n\n");
            for tool in TOOLS.iter() {
                if tool.name.starts_with("config_") {
                    out.push_str(&format!("- `{}`\n", tool.name));
                }
            }
            out
        }
        _ => {
            let mut out = OVERVIEW.to_string();
            out.push_str("\n## Tools\n\n");
            for tool in TOOLS.iter() {
                out.push_str(&format!("- `{}` — {}\n", tool.name, tool.description));
            }
            out.push_str("\nUse `get_guide` with `topic: \"recipes\"` for worked sequences.\n");
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_tool_appears_in_the_guide() {
        let overview = guide(None);
        let tools = guide(Some("tools"));
        for spec in TOOLS.iter() {
            assert!(
                overview.contains(spec.name),
                "{} missing from overview",
                spec.name
            );
            assert!(
                tools.contains(spec.name),
                "{} missing from tools",
                spec.name
            );
        }
        assert!(overview.contains("terminal"));
        assert!(!overview.contains("password="));
    }
}
