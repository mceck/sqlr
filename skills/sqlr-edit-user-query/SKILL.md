---
name: sqlr-edit-user-query
description: Work in the user's open sqlr editor tabs through the sqlr MCP tools - read, fix, rewrite and run the query the user has open, read the result or error of their last run, explain the rows they selected, open a new tab with a proposed query, show objects and plans in the app, find past queries in History and manage Snippets. Use when the user refers to "my query", "this tab", "the error I got", "these rows", asks to open or show something in sqlr, or to save or reuse a snippet.
metadata:
  sqlr-version: 0.1.3
---

# Work in the user's sqlr tabs

The user writes SQL (or MongoDB, Elasticsearch and Redis commands) in sqlr's editor tabs. You can read every open SQL Editor tab, edit it, run it, read its results, and open new tabs, while the user keeps working. Treat a tab as the user's document: change only what they asked for.

If the sqlr tools are missing or failing, use the `sqlr-connect-agent` skill. To query on your own without touching the user's tabs, use `sqlr-query-database`.

## Find the tab

`list_query_tabs` lists the open SQL Editor tabs with id, title, Connection, database, dirty flag, revision and busy flag. When the user says "my query" and several tabs are open, prefer the one they named; otherwise ask. `get_query_tab` returns the text, the revision, the caret and selection (character offsets, end exclusive), and for an exposed Connection:

- `last_run_query_ids`: the results of the tab's latest run, also when the user started it;
- `transaction_open`: whether the tab's Session is inside an open transaction;
- `grid_selection`: the cells the user selected in the result grid, as column, row and value.

Text is readable on every tab. Results, transaction state and the selection need the tab's Connection to be exposed; when they are missing, ask the user to expose the Connection.

## Fix or rewrite the query

1. `get_query_tab` for the current text and `revision`.
2. `edit_query_tab` with `expected_revision` and either `replace` (the whole text) or `edits` (a list of `{start, end, text}` character ranges, applied together).
3. If it answers `conflict`, the user typed in the meantime: read the tab again, redo the edit on the new text, and retry. Never force your old text over theirs.

An edit is one undo step for the user, marks the tab dirty and highlights the change. Prefer small range edits over replacing the whole text, so the user's formatting and comments survive. You never save or close tabs; the user does.

## Help with an error or a result

When the user says "fix the error" or "why zero rows?":

1. `get_query_tab`, then `get_query_result` for each id in `last_run_query_ids`.
2. Read `status`, `error` (code and message), `affected` and `messages`; page rows with `offset` and `limit` if you need them.
3. Edit the tab with the fix and explain it. Run it only if the user asked.

## Run the tab

`run_query_tab` runs the tab's current text in the tab's own Session and database, exactly as the user's Run would:

- `script` runs everything;
- `{statement: n}` runs the n-th statement (zero-based);
- `selection` runs the user's selection.

Run only when the user asked you to. If `transaction_open` is true, say so first: the statement joins the user's open transaction. The answer carries `query_ids`; poll `get_query_result` until `status` leaves `running`. A tab already running answers `busy`; wait instead of retrying in a loop. When the profile asks for confirmation, the call waits while the user decides in sqlr's Approval dialog; `denied` and `timeout` are the user's answer.

Results of a tab belong to the tab. You can page them, but `close_result` and `cancel_query` leave them to the user, and they disappear when the tab runs again or closes.

## Explain the selected rows

`grid_selection` gives the cells the user highlighted. Use them to answer "what are these?", to build a `WHERE` clause matching those rows, or to explain an outlier. Do not echo large selections back verbatim.

## Propose a query in a new tab

`open_query_tab` opens a new tab on an exposed Connection with your text and makes it active, without running it. Use it when the user asks for a query and has no suitable tab, or when you propose something they should review before running (a migration, a bulk update, DDL). One MCP session can open at most five tabs (shared with `show_in_app`); after that, reuse the tabs you opened.

## Show things in the app

`show_in_app` puts something in front of the user:

- `explorer`: reveal and select an object in the Explorer tree (use a path from `browse`);
- `ddl`: open the object's DDL viewer;
- `table_data`: open the object's data;
- `er_diagram`: open an ER diagram of a set of tables;
- `explain`: open the Explain plan of a statement in an existing tab, or of a query in a new tab. With `analyze` the statement runs; a write waits for the user's Approval and is rolled back.

Prefer showing over pasting long DDL or plans into the chat.

## History

`search_history` finds statements the user ran before on exposed Connections, newest first, with Connection, database, time, duration and outcome. Use it for "the query I ran yesterday" and similar, then open it with `open_query_tab` or put it into a tab with `edit_query_tab`.

## Snippets

Snippets are the user's saved statements, global or tied to one Connection, organised in `/`-separated folders.

- `list_snippets` gives names and folders; `get_snippet` gives the SQL.
- `write_snippet` creates one (no id) or updates one; without an id, a Snippet with the same name is replaced, so check the name first.
- `delete_snippet` waits for the user's Approval.

Never store passwords or tokens in a Snippet.
