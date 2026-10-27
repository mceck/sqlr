---
status: done
---

# Agent skills and MCP install

## Specs

- [Agents and terminal](../specs/agents-and-terminal.md)
- [Settings and workspace](../specs/settings-and-workspace.md)
- [Data and schema tools](../specs/data-and-schema-tools.md)
- [Explorer and objects](../specs/explorer-and-objects.md)

## Problem Statement

Connecting an Agent to sqlr is manual and fragile. The person copies a client configuration that contains the MCP token and pastes it into each agent in that agent's own format. When they rotate the token or change the port, every agent breaks silently. Inside the Integrated terminal a project `.mcp.json` works for Claude Code only, asks for a project trust prompt, and duplicates a user-level install when there is one.

Once connected, an Agent learns sqlr from one generic `AGENTS.md` and a `get_guide` tool. Neither teaches a workflow: how to choose the level for a setting, how to create a Connection and check it works, how to fix the query the person has open, or how to explore and query a database safely. The guide exists only inside sqlr, so an agent working elsewhere has no way to learn sqlr.

The tool surface also has gaps and two defects:

- an Agent cannot see the outcome of a run in the person's tab;
- it cannot open a tab, show something in the app, read DDL, explain a query or search History;
- it cannot test a Connection it created;
- `describe_object` always returns empty index and foreign-key lists;
- **security defect:** `config_write` and `config_delete` change Connections that are not exposed. Their ids leak through the settings listing, so an Agent can point a non-exposed production Connection at a host it controls and receive the saved password on the next connect;
- **approval defect:** `run_query_tab` opens the person's own confirmation instead of the Agent Approval dialog, answers `approval_required`, and tells the Agent to call again. That can run the statement twice.

Saved tasks are dead code. Nothing in the app creates them, yet the MCP lists and deletes them.

## Solution

**Settings → Agents** lists the supported agents found on the machine. **Install** writes sqlr's server entry into the agent's user-level configuration, **Remove** takes it out, and sqlr keeps every Agent installation current when the token rotates or the port changes. **Copy MCP config** stays as a fallback for other clients.

sqlr ships five **sqlr skills**: `sqlr-configure`, `sqlr-create-connection`, `sqlr-edit-user-query`, `sqlr-query-database` and `sqlr-connect-agent`. They live in the repository's `skills/` folder, where `npx skills add mceck/sqlr` can install them, and the app copies them into the Terminal workspace. `get_guide` is removed: the initialize instructions carry a short overview and point to the skills. The Terminal workspace loses `.mcp.json` and keeps a shorter `AGENTS.md`.

The MCP surface grows to 24 tools:

- `open_query_tab`, `show_in_app`, `test_connection`, `get_ddl`, `explain_query` and `search_history` are new;
- `get_query_tab` reports the tab's last run, its open transaction and the grid selection;
- tab runs return result ids that `get_query_result` pages;
- `describe_object` returns indexes and foreign keys.

Configuration writes respect exposure, `run_query_tab` goes through the Agent Approval dialog, and Saved tasks are removed.

## User Stories

1. As a person using Claude Code, I want Settings → Agents to show that Claude Code is installed on my machine, so that I know sqlr can connect it.
2. As a person, I want one Install button per agent, so that I never hand-edit an agent's configuration file.
3. As a person, I want Install to tell me once that it writes the sqlr token into another program's file, so that I consent knowingly.
4. As a person, I want Install to keep the rest of my agent configuration exactly as it was, comments included, so that sqlr never damages my other MCP servers.
5. As a person, I want a backup of my agent configuration before sqlr first changes it, so that I can restore it by hand if something goes wrong.
6. As a person who rotates the MCP token, I want every agent I installed to receive the new token automatically, so that rotation does not break my agents.
7. As a person who changes the MCP port, I want every Agent installation updated, so that my agents keep finding the server.
8. As a person, I want an Outdated state when an installed entry no longer matches the running server, with an Update action, so that I can repair it in one click.
9. As a person whose agent configuration sqlr cannot parse, I want a Manual setup state with a Copy config action in that agent's format, so that I can still connect it.
10. As a person, I want Remove to delete only sqlr's entry, so that uninstalling sqlr from an agent leaves the rest of its configuration alone.
11. As a person, I want a reminder to restart the agent after Install or Update, so that I know why the tools are not there yet.
12. As a person who turns the MCP server off, I want my Agent installations to stay, so that turning it back on reconnects them.
13. As a person using an unsupported agent, I want Copy MCP config to remain available, so that any MCP client can still connect.
14. As a person, I want Install to be available only while the server runs, so that the written token is always valid.
15. As a person, I want the agent list checked in the background, so that Settings never stalls on file access.
16. As a person who opens the Integrated terminal, I want the sqlr skills already present in its workspace, so that the agent I start there knows how to work with sqlr.
17. As a person, I want the workspace to keep its own `.agents/skills` and `.claude/skills` layout per skill, so that skills I add with `npx skills` sit next to sqlr's without conflict.
18. As a person upgrading sqlr, I want the sqlr skills in the workspace replaced by the new version, so that agents follow the current tool surface.
19. As a person, I want skills I add myself left untouched, so that sqlr manages only its own `sqlr-*` skills.
20. As a person upgrading sqlr, I want the generated `.mcp.json` removed from the Terminal workspace, so that Claude Code does not see a second, conflicting sqlr server.
21. As a person who edited `.mcp.json` by hand, I want it left in place, so that sqlr never deletes my work.
22. As a person working outside sqlr's terminal, I want to install the sqlr skills from the repository with `npx skills add`, so that any agent anywhere can learn sqlr.
23. As a person, I want to install a single skill from the repository, so that I take only what I need.
24. As an Agent, I want the initialize instructions to explain exposure, Approvals, tighten-only safety and write-only secrets, so that I behave correctly even without the skills.
25. As an Agent, I want the initialize instructions to tell me how to get the skills, so that I can point the person to them.
26. As an Agent asked to change a setting, I want `sqlr-configure` to tell me which level to write, so that a per-Connection request does not change every Connection.
27. As an Agent, I want `sqlr-configure` to map common requests to setting keys, so that I find the right setting without scanning the whole catalogue.
28. As an Agent, I want `sqlr-configure` to say when each kind of change takes effect, so that I can tell the person whether a restart is needed.
29. As an Agent, I want `sqlr-configure` to explain profiles, keymap and folders, so that I can create a profile, rebind a shortcut or move a Connection.
30. As an Agent asked to add a Connection, I want `sqlr-create-connection` to walk me through the driver manifest, the values, the secrets and a test, so that the Connection works before I hand it back.
31. As an Agent, I want `test_connection` on a Connection I just created, so that I can fix a wrong host or password myself.
32. As an Agent, I want `test_connection` to name missing secret fields, so that I ask the person for exactly what is missing.
33. As a person, I want an Agent-created Connection to start with Agent access None and the Agent to tell me so, so that exposure stays my decision.
34. As an Agent asked to fix the person's query, I want `sqlr-edit-user-query` to explain revisions, conflicts, busy tabs and undo, so that I never overwrite the person's edits.
35. As an Agent, I want `run_query_tab` to return result ids, so that I can read what the run produced.
36. As an Agent, I want `get_query_tab` to list the ids of the tab's last run even when the person started it, so that I can help with an error the person just hit.
37. As an Agent, I want `get_query_result` to page a tab's result with the same envelope as `run_query`, so that I learn one result shape.
38. As a person, I want `close_result` to leave my tab's results alone, so that an Agent cannot clear my grid.
39. As a person, I want tab results and errors visible to Agents only on exposed Connections, so that data from a non-exposed Connection never reaches an Agent.
40. As an Agent, I want to know whether the person's tab has an open transaction, so that I warn before running in it.
41. As an Agent, I want the person's grid selection, so that I can explain or work on the rows they point at.
42. As an Agent, I want to open a new query tab with text for the person, so that I can propose a query without overwriting one of theirs.
43. As a person, I want at most five tabs opened by one Agent session, so that an Agent cannot flood my workspace.
44. As an Agent, I want to search History on exposed Connections, so that I can pick up a query the person ran earlier.
45. As a person, I want History entries of non-exposed Connections hidden from Agents, so that exposure covers History too.
46. As an Agent, I want `show_in_app` to reveal an object in the Explorer, open its DDL, open its data, open an ER diagram or open an Explain plan, so that I can show the person something instead of describing it.
47. As an Agent querying on my own, I want `sqlr-query-database` to describe schema discovery, one-statement queries, paging, closing results, limits and transactions, so that I query efficiently and leave nothing open.
48. As an Agent, I want `describe_object` to return indexes and foreign keys, so that I understand relationships without dialect-specific catalog queries.
49. As an Agent, I want `get_ddl` for any object the Explorer shows, so that I can read its definition as the app writes it.
50. As an Agent, I want `explain_query` with an optional analyze, so that I can study a plan without leaving writes behind.
51. As an Agent on a `Read` Connection, I want analyze refused for writes, so that read exposure is never bypassed.
52. As an Agent whose tools are missing or failing, I want `sqlr-connect-agent` to diagnose the cause and the next step, so that I guide the person instead of retrying blindly.
53. As an Agent, I want `sqlr-connect-agent` to tell me where the Discovery file is, so that I can check whether sqlr is running without touching the token.
54. As a person, I want an Agent request to run a write in my tab to show the Agent Approval dialog, so that I know an Agent is asking and denial is the default.
55. As a person, I want an approved tab run to execute exactly once, so that a `DELETE` is never run twice.
56. As a person, I want an Agent unable to change or delete a Connection I did not expose, so that it cannot redirect a saved password.
57. As a person, I want Agent-facing settings values free of non-exposed Connection ids, so that their existence does not leak.
58. As a maintainer, I want a test that fails when a tool is not covered by any skill or a skill cites a tool or setting that does not exist, so that the skills stay in step with the registry.
59. As a maintainer, I want Saved tasks removed from code, store, specs and glossary, so that no dead surface remains.

## Implementation Decisions

### Agent installation (app)

- A new app-side agent installation module owns discovery, install, update, remove and state. It runs on a background worker; the UI thread reads its last snapshot and sends commands. It depends on the user's home and platform configuration directories, injected so tests can point it at a temporary home.
- Supported agents and their user-level files:

  | Agent | File | Entry |
  |---|---|---|
  | Claude Code | `~/.claude.json` | `mcpServers.sqlr` = `{type: "http", url, headers: {Authorization}}` |
  | Codex CLI | `~/.codex/config.toml` | `[mcp_servers.sqlr]` with `url` and `http_headers` |
  | Gemini CLI | `~/.gemini/settings.json` | `mcpServers.sqlr` = `{httpUrl, headers}` |
  | Cursor | `~/.cursor/mcp.json` | `mcpServers.sqlr` = `{url, headers}` |
  | VS Code | the Code user directory's `mcp.json` | `servers.sqlr` = `{type: "http", url, headers}` |
  | OpenCode | `~/.config/opencode/opencode.json` | `mcp.sqlr` = `{type: "remote", url, headers}` |

- Discovery checks each agent's known configuration directory and known installation paths without running processes or reading `PATH`. A supported agent found by either check is a Detected agent.
- An agent's state is one of:
  - `NotDetected`;
  - `NotInstalled`;
  - `Installed`: the entry matches the running server's URL and token;
  - `Outdated`: a `sqlr` entry exists but differs;
  - `ManualSetup(reason)`: the file exists and cannot be parsed or has an unexpected shape.
- Writes are format-preserving. TOML goes through a document editor that keeps formatting, and JSON and JSONC go through an editor that keeps comments, order and unrelated keys. Each write goes to a temporary file in the same directory, is renamed into place, and keeps the original permissions; a new file is owner-only on Unix. Before the first change to a file, a copy is saved next to it as `<file>.sqlr-backup`, unless one already exists. Remove deletes only the `sqlr` entry and leaves an emptied container map in place.
- An installation registry in the data directory records each Agent installation by agent and file. It carries no token. On token rotation or port change, sqlr rewrites every recorded installation with the new URL and token. A failed rewrite turns that installation `Outdated` and logs the agent and the error, without the token. Remove drops the record.
- Disabling the server leaves installations in place.
- The registry also notices an agent rewriting its own file: state is recomputed when Settings → Agents opens and after each action, so a `sqlr` entry that disappeared reads `NotInstalled`.

### Settings → Agents

- Copy MCP config moves under the list as "Other agent…". The agent list sits below the server status: one row per supported agent with name, file path, state and action (Install, Remove, Update, Copy config).
- Install, Update and Copy config are disabled while the server is not running.
- The first write to an agent's file shows a one-time confirmation that names the file and says it receives the sqlr token.
- After Install or Update, a toast says "Restart <agent> to load sqlr".
- The guide link points to the repository's `skills/` folder.

### sqlr skills

- The skills are hand-written in the repository's `skills/` folder, one folder per skill with a `SKILL.md` and optional `references/`. They follow the Agent Skills format, so `npx skills add mceck/sqlr` and per-skill installs work. They describe workflows and rules and do not repeat tool argument lists, which the tool schemas carry.
- Every `SKILL.md` frontmatter carries `metadata.sqlr-version`, equal to the workspace version.
- Contents:
  - **`sqlr-configure`**: Setting levels and how to choose one (app-wide options are global only; "this Connection" is the Connection level; "all production Connections" is the profile; ask when ambiguous). Tighten-only Safety flags and profile moves. A short map from common intents to setting keys. When changes apply: immediately, on terminal Restart, or on the next Session. Profiles, keymap with reset, folders and moving Connections. What stays human-only.
  - **`sqlr-create-connection`**:
    1. read the driver manifest;
    2. ask the person for values, never guess, and accept a Connection URL;
    3. create with the right folder and profile;
    4. write secrets and agree `save_secrets`;
    5. run `test_connection`, fix and retry;
    6. tell the person Agent access is theirs to set.

    It also covers `secret_required` on a Connection the Agent created.
  - **`sqlr-edit-user-query`**: Listing and reading tabs, revisions and `conflict`, `busy`, undo as one step, run targets, reading the last run through result ids, the open-transaction warning, the grid selection, `open_query_tab`, `show_in_app`, History search and Snippets.
  - **`sqlr-query-database`**: Schema discovery with `search_objects`, `describe_object`, `browse` and `get_ddl`. `run_query` runs one unit with an explicit row limit and no `SELECT *` on unknown tables. Paging, `close_result`, limits and deadlines. Modifications go through `run_query` on `Write` exposure, governed by the profile and Approvals. Transactions: read the profile's commit mode, `COMMIT` explicitly, leave nothing open. `explain_query`. `references/` hold one file per family (SQL, MongoDB, Elasticsearch, Redis).
  - **`sqlr-connect-agent`**: Diagnosis in order:
    - tools missing → Install from Settings → Agents, then restart the agent; Copy MCP config for an unsupported agent;
    - connection refused → sqlr is closed or the server is off;
    - `401` → restart the agent, or Update when Settings shows Outdated;
    - `not_exposed` or no Connections → the person sets Agent access;
    - `read_only`, `denied` and `timeout` are decisions, not faults.

    It gives the Discovery file location per platform for read-only checks, and forbids editing the agent's own MCP configuration by hand, asking for or printing the token, and looking for the vault.
- The binary embeds the skills from `skills/`, so the repository is the single source. The MCP crate exposes the embedded set, because the consistency test lives next to the registry; the app's Terminal workspace writes them.

### Terminal workspace

- Each sqlr skill is written to `.agents/skills/<name>/`, and `.claude/skills/<name>` is a relative symlink to it; on Windows it is a copy. There is no lock file.
- On every preparation the worker rewrites a `sqlr-*` skill whose content differs from the embedded one, and removes `sqlr-*` folders and links that are no longer in the set. Other entries are never touched.
- `.mcp.json` is no longer written. When the file contains only a `sqlr` entry or an empty `mcpServers` map, the worker deletes it; any other content stays. Refreshes on server state changes go away with it.
- The `AGENTS.md` template is replaced by a short one: what the folder is, that sqlr is reached through MCP once the agent is installed from Settings → Agents, to use the `sqlr-*` skills, and that the person owns the file. It is still created only when missing, and the `CLAUDE.md` link is unchanged.

### MCP surface

- `get_guide` and the guide module are removed. The initialize `instructions` are generated in code as a short overview:
  - what sqlr is, and that the server runs only with the app;
  - that only exposed Connections are visible and the person decides exposure;
  - Approvals and tighten-only safety;
  - write-only Connection values and secrets;
  - the `npx skills add mceck/sqlr` command and the fact that the Integrated terminal already holds the skills.
- New tools:
  - **`open_query_tab(connection_id, database?, text, title?)`** opens a new unsaved SQL Editor tab next to the active one and makes it active, with a toast. The Connection must be exposed. It returns `tab_id` and `revision`, and runs nothing.
  - **`show_in_app(connection_id, view, …)`**. Every view needs an exposed Connection, makes no change, brings the view to the front, shows a toast, and returns a `tab_id` when it opens a tab. The views are:
    - `explorer` with a `browse` path: expand and select;
    - `ddl` with a path: the DDL viewer;
    - `table_data` with a path: the object's data tab;
    - `er_diagram` with `database?`, `schema?` and `tables`;
    - `explain` with either `tab_id`, `statement?` and `analyze?` (the Plan pane of an existing tab), or `query` and `analyze?` (a new tab with the query, then Explain).
  - **`test_connection(connection_id)`** uses the connection dialog's temporary test Session and does not show the Connection in the Explorer. It returns `{ok: true, server_version}` or `{ok: false, error: {code, message}}`, with codes `auth`, `connection`, `tls` and `timeout` and a sanitized message, and `secret_required` with field names.
  - **`get_ddl(connection_id, path)`** returns the `CREATE` script the DDL viewer shows for the node at a `browse` path.
  - **`explain_query(connection_id, query, analyze?)`** runs the engine's Explain on the Agent Session and returns times, cost, estimated and actual rows, warnings, the operator mix and the raw plan, truncated at 256 KiB.
  - **`search_history(query?, connection_id?, limit?)`** returns at most 200 entries, 50 by default: SQL, Connection, database, time, duration and outcome. Only entries of exposed Connections are searched, before the limit.
- Explain rules, shared by `explain_query` and `show_in_app` with `explain`:
  - one statement only;
  - `unsupported` when the Driver lacks Explain;
  - `analyze` on a `Read` exposure is refused for a modifying statement;
  - on a write, `analyze` needs an Approval and runs in a transaction that is always rolled back;
  - `busy` while a transaction is open.
- An Agent session may open at most five tabs, counted across `open_query_tab` and the tab-opening `show_in_app` views. The sixth returns `busy` with a message to reuse the open tabs.
- Tab results:
  - `run_query_tab` answers `{tab_id, status, query_ids}`, one id per executed statement;
  - `get_query_tab` adds `last_run_query_ids`, `transaction_open` and `grid_selection`. The selection lists `{column, row, value}` per selected cell in the `run_query` value mapping, capped at 1000 cells and 256 KiB;
  - `get_query_result` accepts tab ids and returns the `run_query` envelope;
  - a tab result is owned by the tab: `close_result` answers `invalid`, it has no TTL, and ids become `not_found` when the tab runs again or closes;
  - result ids, `last_run_query_ids` and `grid_selection` are present only when the tab's Connection is exposed.
- The host interface gains the calls these need: opening tabs and views, the tab run result ids, the transaction state, the grid selection, and resolving a tab result page. Each is answered on the UI thread through the existing request queue.
- `describe_object` fills `indexes` (name, columns, unique, method, predicate) and `foreign_keys` (name, columns, referenced table and columns, update and delete actions) from the engine's object definition loader.
- Exposure rule for configuration: `config_write` and `config_delete` on an existing Connection require that Connection to be exposed or created by the same MCP session. Otherwise they answer `not_exposed`, as they would for an unknown id. The server tracks the Connection ids each MCP session created.
- `config_list` with `settings` drops `connection:<id>` values for non-exposed Connections.
- `run_query_tab` approval: a run that needs confirmation goes through the Agent Approval dialog and blocks like `run_query`, with a 60-second timeout and Deny as the default. Allowed runs return `query_ids`; otherwise the answer is `denied` or `timeout`. `approval_required` is removed.

### Saved tasks removal

- The Saved task and Task run model is removed from the core, along with its persistence in the store, its execution in the engine, the `tasks` target of `config_list`, the `task` target of `config_delete`, and the host interface calls.
- A new store schema version drops the task and task run tables.
- The data and schema tools spec loses its Saved tasks section, and the glossary loses both terms.

### Specs

- The agents and terminal spec is rewritten for installs, skills, the Terminal workspace, the 24-tool surface, the new contracts and the fixes.
- The settings and workspace spec describes the Agents section.
- The data and schema tools spec drops Saved tasks.
- The explorer and objects spec mentions that `get_ddl` and `show_in_app` reuse the DDL viewer and the object views.

## Testing Decisions

- **MCP protocol seam** (the HTTP server with the test host and a real SQLite engine): the initialize instructions, the absence of `get_guide`, the configuration exposure rule including the created-in-session exception, filtered settings values, `describe_object` indexes and foreign keys, `get_ddl`, `explain_query` (read, analyze, write refusal on `Read`), `search_history` filtering, `test_connection` success, failure and missing secrets, `open_query_tab` and `show_in_app` argument validation and the tab limit, and the tab-result id contract through the test host. Prior art: the existing exposure, approvals and connection-write tests.
- **App seam** (`egui_kittest` against the real app host): `run_query_tab` shows the Agent Approval dialog, runs once when allowed and answers `denied` or `timeout`; tab results are readable and survive `close_result`; `open_query_tab` and every `show_in_app` view open the expected tab or selection; `transaction_open` and `grid_selection`; the Settings → Agents list with each state and action. Prior art: the agents UI tests.
- **Agent installation seam (new)**: the installation module against a temporary home with fixture files for the six formats. It covers detection and non-detection, install into missing, empty and populated files, preservation of comments and unrelated keys, the backup created once, remove, outdated and update, manual setup for a broken file, and rewrite of recorded installations on rotation and port change.
- **Terminal workspace seam**: skills written with per-skill links, changed skills rewritten, stale `sqlr-*` removed, foreign skills untouched, the generated `.mcp.json` deleted and an edited one kept, and the new `AGENTS.md` created only when missing. Prior art: the existing workspace tests.
- **Skill consistency** (in the MCP crate): every registry tool appears in at least one skill, every tool named in a skill exists, every setting key named in a skill exists in the settings catalogue, and every `sqlr-version` equals the crate version.
- **Store**: the schema migration drops the task tables on an existing database. The Saved task tests in core, store, engine and MCP are removed with the code.
- Per the project's testing preferences, each step runs its focused tests; the full workspace suite runs once at the end.

## Acceptance Criteria

- [x] Settings → Agents lists the six supported agents with their state, and Install, Remove, Update and Copy config behave as described.
- [x] Install writes a working `sqlr` entry in each format and preserves every other byte of meaning, including comments in JSONC and formatting in TOML.
- [x] A `.sqlr-backup` is written before the first change to an agent file and never overwritten.
- [x] Token rotation and port change rewrite every recorded installation; a failure shows Outdated.
- [x] Copy MCP config stays available as "Other agent…".
- [x] The repository has `skills/sqlr-configure`, `skills/sqlr-create-connection`, `skills/sqlr-edit-user-query`, `skills/sqlr-query-database` and `skills/sqlr-connect-agent`, each with a valid `SKILL.md` carrying `metadata.sqlr-version`.
- [x] The README documents `npx skills add mceck/sqlr`.
- [x] The Terminal workspace holds the five skills under `.agents/skills` with per-skill links in `.claude/skills`, rewritten on change and pruned when stale, with no lock file.
- [x] The Terminal workspace no longer writes `.mcp.json`, and deletes the generated one.
- [x] The new `AGENTS.md` template is used when the file is missing.
- [x] `get_guide` is gone, and the initialize instructions carry the overview and the skills command.
- [x] The tool surface has 24 tools, including `open_query_tab`, `show_in_app`, `test_connection`, `get_ddl`, `explain_query` and `search_history` with the contracts above.
- [x] `run_query_tab` returns `query_ids`; `get_query_tab` returns `last_run_query_ids`, `transaction_open` and `grid_selection` for exposed Connections only; `get_query_result` pages tab results; and `close_result` does not close them.
- [x] `describe_object` returns indexes and foreign keys for relational Connections.
- [x] `config_write` and `config_delete` refuse non-exposed Connections not created by the session, with `not_exposed`.
- [x] Settings values carry no non-exposed Connection id.
- [x] `run_query_tab` uses the Agent Approval dialog, runs at most once, and no longer returns `approval_required`.
- [x] An Agent session cannot open more than five tabs.
- [x] Saved tasks are gone from code, store, MCP, specs and glossary, and the store migration drops their tables.
- [x] The skill consistency test passes, and fails when a tool is added without a skill mentioning it.
- [x] The affected specs describe the new behaviour.
- [x] `cargo clippy --workspace --all-targets -- -D warnings` and the full test suite pass.

## Out of Scope

- Windsurf, Zed, JetBrains AI and Claude Desktop installs, and a stdio bridge.
- Installing skills at user level from the app.
- Migrating an existing customised `AGENTS.md`.
- Sessions and locks through MCP, and running dumps, restores or transfers through MCP.
- The workflow skills `sqlr-tuning`, `sqlr-migrations` and `sqlr-data-analysis`.

## Further Notes

- The follow-up task holds Sessions and locks, the workflow skills and the additional agents.
- The skills cite only tools from this task's final surface, so they are written after the MCP changes land.
