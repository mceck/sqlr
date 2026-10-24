# Working with sqlr

This is the integrated terminal's persistent workspace, in the `terminal` folder
inside sqlr's configuration directory. Files here survive hiding and restarting
the terminal and closing sqlr. sqlr creates this guide only when it is missing,
so you can keep additional instructions here.

## Connect to the sqlr MCP server

`.mcp.json` in this directory contains the active sqlr MCP server URL and its
authentication credentials. Use it with clients that support project MCP config.
For other clients, use **Copy MCP config** in **Settings → Agents** and import the
configuration in the client's supported format.

If the server configuration is empty, ask the user to enable MCP in **Settings →
Agents**. sqlr refreshes `.mcp.json` when the server starts, stops, changes port or
rotates its token. Reconnect or reload your client's MCP configuration afterwards.
The server is available only while sqlr is running. No token is injected into the
shell environment. Do not print, log, commit or share `.mcp.json` or its credentials.

## Required first call

At the start of every MCP session, call `get_guide({})` and read it before using any
other tool. Request `get_guide({"topic":"tools"})` for detailed arguments and
`get_guide({"topic":"recipes"})` for examples.

1. Discover exposed connections with `config_list({"target":"connections"})`.
   Use the returned connection IDs, not display names. If none are exposed, ask the
   user to grant agent access in sqlr; do not guess credentials or connection IDs.
2. Explore with `browse({"connection_id":"<id>","path":[]})`, then pass returned
   paths unchanged to expand nodes. Use `describe_object` to inspect table details.
3. Use `run_query` for a statement or native command and directly pageable results.
   Check `status` and `error`, page with `get_query_result`, and release retained
   results with `close_result` when finished.
4. For work in the user's editor, use `list_query_tabs` and `get_query_tab` before
   `edit_query_tab` or `run_query_tab`; respect revision and busy-state checks.

Use sqlr's MCP tools to work with the user's database context. Connection exposure,
read/write permissions, transaction rules and destructive-action approvals remain
controlled by sqlr. Database credentials are write-only and must not be requested.
