//! The Markdown guide, generated from the tool registry so it cannot drift.

use crate::registry::TOOLS;

const OVERVIEW: &str = "\
# sqlr and agents

sqlr is a native database client. This MCP server exists only while the application is
running: there is no daemon and no endpoint after it quits. The server listens on
`127.0.0.1` only.

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

Destructive statements and configuration or snippet deletes open the same in-app
approval dialog the person already sees. It names you (client name and version), the
connection, database and exact statement, and it times out after 60 seconds with
`denied` and reason `timeout`. A read-only or non-exposed session never reaches an
approval. Secret writes never ask.

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
  lives in the OS keychain, never in TOML, the discovery file, logs or tool output.
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

Call `get_guide` first. All tools are flat and scoped to this sqlr server. The terminal
has no tool.

| Tool | Arguments |
|---|---|
";

const RECIPES: &str = "\
# Recipes

## Discover a schema

1. `config_list` with `target: \"connections\"` — pick an exposed connection.
2. `browse` with its id and an empty path — the root children.
3. `describe_object` for a relational table (columns, keys, defaults).
4. `search_objects` when you know part of a name.

## Write and run a query

1. `run_query` with one statement and `max_rows` as needed.
2. Read the typed columns and rows from the tagged result.
3. If `truncated` is true, call `get_query_result` with the same `query_id` and an
   `offset` to continue.
4. `close_result` when done.

## Fix an open tab

1. `list_query_tabs` — SQL query tabs only.
2. `get_query_tab` — text, revision, caret and selection.
3. `edit_query_tab` with `expected_revision`; on `conflict`, re-read and retry.
4. `run_query_tab` with `target: \"script\"`, `{\"statement\": n}` or `\"selection\"`.
5. The person sees the edit highlighted and can undo it in one step.

## Save a snippet

`write_snippet` with a name and SQL; include `folder` for a `/`-separated folder and
`conn_id` for a connection scope. `delete_snippet` asks for approval.

## Create a connection

1. Read the driver manifest with `config_list` and `target: \"drivers\"`.
2. `config_write` with `target: \"connection\"`, `name` and `driver`, plus `values`.
3. Write secrets with the `secrets` map. They are accepted but never returned.
4. Exposure stays `None` until the person sets it.

## Set up an agent in the terminal

Open the terminal (`Ctrl+Backtick`), use **Copy MCP config** in its header, and paste the
configuration into Claude Code, Codex or another client. No token is injected into the
shell environment and no agent is started automatically.
";

pub fn guide(topic: Option<&str>) -> String {
    match topic.unwrap_or("overview") {
        "security" => SECURITY.to_string(),
        "tools" => {
            let mut out = TOOLS_INTRO.to_string();
            for tool in TOOLS.iter() {
                out.push_str(&format!("| `{}` | {} |\n", tool.name, tool.arguments));
            }
            out.push('\n');
            out.push_str("Descriptions are also in `tools/list`. No tool exposes SQL text\nin logs or secrets in any form.\n");
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
readable through MCP; `terminal.cwd` and `terminal.scrollback` are ordinary settings.\n\
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
