---
status: done
---

# Query editor, autocomplete, History and Snippets

## Specs

- [Query editor](../specs/query-editor.md)
- [Query execution](../specs/query-execution.md)
- [Explorer and objects](../specs/explorer-and-objects.md)
- [Drivers](../specs/drivers.md)
- [Settings and workspace](../specs/settings-and-workspace.md)

## Problem Statement

Writing SQL in a database client is where most of the work happens, and the tools people come from impose the wrong costs. A simple text field re-lays out the whole document on every keystroke, so a 10 MB script stutters. Highlighting that re-parses the file freezes the window. Without multi-cursor, grouped undo or find and replace, repetitive edits are tedious. Completion that does not know the schema offers the wrong names, and one that parses the entire script stalls the frame. A tab that is not tied to a specific Connection and database runs against whatever happens to be active. Once a query is gone from the editor, there is no way to find it again or to keep the SQL a user runs every day.

## Solution

A dedicated Query editor widget holds the text in a rope and lays out only the visible lines. A resumable per-line lexer colors SQL and the other Query languages without parsing the document, and a grammar parses only the statement around the caret, off the UI thread above 64 KiB, to power schema-aware completion. The editor provides multi-cursor, grouped undo, find and replace, auto-indent, bracket pairing, toggle comment, soft wrap, line numbers, a tint on the statement under the caret and underlines for syntax errors. Each Editor tab is bound to a Connection and, when chosen, a server database, shown in the tab and its breadcrumb. Every executed statement goes to a searchable History stored locally, and named Snippets in folders can be global or tied to one Connection.

## User Stories

1. As a user writing a long script, I want the editor to stay responsive as the file grows, so that typing never stalls.
2. As a user with a 1 MB line, I want a keystroke to cost the same as on a short line, so that generated SQL stays workable.
3. As a user, I want SQL highlighted according to my database's dialect, so that keywords, functions, comments and quoted names look right.
4. As a user of MongoDB, Elasticsearch or Redis, I want the console text highlighted and split for that language, so that each tab behaves the way I expect.
5. As a user, I want small scrolls and edits to reuse cached layout, so that the editor stays smooth.
6. As a user, I want to place several carets and type once, so that I can edit repeated lines together.
7. As a user, I want undo and redo to work in sensible groups, so that correcting a word takes one undo.
8. As a user, I want undo to restore my carets, so that multi-caret edits are predictable.
9. As a user, I want find and replace with match case and whole word, so that I can fix repeated names precisely.
10. As a user, I want find to wrap around the document, so that I never wonder where the search went.
11. As a user, I want auto-indent and bracket pairing, so that writing nested queries stays comfortable.
12. As a user, I want to indent and outdent blocks, toggle line comments and duplicate a line, so that restructuring a query is quick.
13. As a user, I want to format the statement, so that a messy paste becomes readable.
14. As a user, I want soft wrap and line numbers as independent toggles, so that the view fits how I work.
15. As a user, I want the statement under the caret tinted, so that I always know what Run will execute.
16. As a user, I want a syntax error underlined at its position with the message on hover, so that I can fix it without reading the result.
17. As a user, I want schema-aware completion after `FROM`, `JOIN`, a table alias and a schema name, so that I do not have to remember table and column names.
18. As a user, I want keywords and functions from my dialect with the caret inside a function's parentheses, so that completing is faster than typing.
19. As a user, I want completion to quote identifiers that need quoting, so that an inserted name always works.
20. As a user, I want completion on huge statements not to freeze the editor, so that I can keep typing.
21. As a user, I want Ctrl+Space to open completion on demand, so that I can ask for suggestions before typing a prefix.
22. As a user, I want ⌘↵ to run the statement under the caret or my selection, so that I control what runs.
23. As a user, I want ⇧⌘↵ to run the whole script, so that I can execute a file in one action.
24. As a user, I want each Editor tab bound to one Connection, so that a run never goes to the wrong server.
25. As a user, I want to see a tab's Connection and database at a glance, so that I trust what it runs against.
26. As a PostgreSQL user, I want to choose another database for a tab from its breadcrumb, so that one Connection can serve several databases.
27. As a user, I want switching a tab's Connection to switch its syntax, so that highlighting follows the database.
28. As a user, I want my query history searchable and filterable by Connection, so that I can find a statement I ran last week.
29. As a user, I want History entries to show when, where, how long and how the statement ended, so that I can tell a good run from a failed one.
30. As a user, I want to reopen a History entry on its original Connection and database, so that it runs where it ran before.
31. As a user, I want to insert History SQL into the editor, copy it, or clear history for one Connection or all, so that I control it.
32. As a user, I want statements that set passwords or API keys never stored in plaintext History, so that a secret cannot leak into a local file.
33. As a user, I want to save SQL as a named Snippet in a folder, globally or for one Connection, so that my everyday queries are one click away.
34. As a user, I want Snippets listed per folder with a filter, so that a growing collection stays navigable.
35. As a user, I want the snippet dialog to suggest a name from the SQL, so that saving takes one confirmation.
36. As a user, I want Snippets inserted or opened in a tab, edited and deleted with confirmation, so that I can manage them from the panel.
37. As a user, I want a clear warning when History and Snippets cannot be saved, so that I know before I rely on them.
38. As an agent user, I want statements edited and run through MCP to show in the editor like my own edits, so that I can see what changed.

## Implementation Decisions

- The editor widget owns the text as a rope, a syntax layer, a per-line layout cache, the completion popup, the find bar and the workers. It returns text changes, catalog needs and a "save as snippet" request; it calls no engine.
- The syntax layer stores the lexer state each line starts in, lexes toward what is visible under a 4 ms per-frame budget, and stops after an edit once the state converges. Lines over 4,096 bytes keep checkpoints every 4,096 bytes.
- Core provides the Dialect-aware lexer and splitter, the Query language lexers and splitters, identifier quoting and unquoting, and the History and Snippet records. The UI owns the per-line syntax layer and the statement scope parser.
- Statement boundaries come from the lexer; the statement scope (relations with schema, name and alias, plus CTE names) comes from a tree-sitter grammar over the caret's statement. A statement over 64 KiB, or a caret line over 64 KiB, goes to a background scope worker with a 32 KiB window on each side; the previous scope stays in use meanwhile.
- Completion has a context (off, relation, member, general) computed from at most 4,096 bytes before the caret, a candidate builder over the scope and the Catalog cache, and case-insensitive prefix-then-substring ordering capped at 200 items. Missing catalog pieces become requests emitted once per popup session.
- The edit model applies one planned edit per caret, normalizes and merges carets, and records undo groups. Typing and backspacing merge while contiguous in place, within one second and of the same kind; other commands form their own group; the stack keeps 10,000 groups.
- Find scans line by line and wraps; replace all applies as one undo group.
- The app owns Editor tabs with a Connection and optional database, the breadcrumb and its database menu, the run actions and their confirmations, and the History and Snippets panels and dialogs.
- History is recorded by the engine when a Result first leaves the Running state, then written through the store's ordered queue. SQL is cut at 1 MiB; the store keeps the newest 100,000 rows. Statements whose text may hold a secret are not recorded.
- Snippets live in memory in the engine, sorted by folder and name, and are written to the local database behind, with early deletes remembered until the startup load lands. The store owns the History and Snippet tables.
- History searches run on the store worker and are answered into per-channel views (panel and palette), with tokens so a newer query or response wins.

## Testing Decisions

- Seam: the editor widget's state and the app-level editor tests through `egui_kittest`, injecting keys, clicks and text. Prior art: the editor interaction suite covering typing, clicks, multi-caret, IME, undo, comment toggle, find and replace, markers, completion, run targets, soft wrap, tint and the text menu.
- Seam: the core lexer and splitter, directly. Prior art: the syntax suite for lexer states, nested comments, dollar quotes, escape strings, MySQL and SQLite quotes, splitting and `is_ddl`, and the language suite for Mongo shell, JSON and Elasticsearch splitting.
- Seam: the statement scope parser and its worker, directly. Prior art: scope tests for aliases, CTEs, quoted relations, delete and insert targets, the placeholder rule, the unknown grammar, and the worker's large-statement and rope-window paths.
- Seam: the syntax layer and completion builder, directly. Prior art: tests for line-state resumption, long-line checkpoints, convergence after an edit, statement regions, completion contexts, alias columns, default schemas, schema qualifiers, quoting, ordering and the item cap.
- Seam: the engine library with the shared local database. Prior art: history entry truncation and the password-bearing exclusion, snippet restore and early deletes, search refresh and per-channel tokens.
- Seam: the store's database. Prior art: history add/search/clear, snippet save/list/delete and trimming.
- Seam: the app's panels and dialogs. Prior art: the library tests for memory fallback, reopened entries binding their Connection and database, snippet name suggestion and dialog behaviour.

## Acceptance Criteria

- [x] The editor lays out only visible lines and caches line layouts, with long lines laid out over a window.
- [x] Highlighting uses a resumable per-line lexer per Query language, with checkpoints in long lines and a bounded per-frame budget.
- [x] SQL highlighting follows each Connection's Dialect, and MongoDB, Elasticsearch, Redis and JSON tabs lex with their own rules.
- [x] Multi-cursor typing, vertical goal columns, merged overlapping carets and caret restoring undo work.
- [x] Undo groups typing and backspacing by locality and time, keeps 10,000 groups, and a new edit clears redo.
- [x] Find and replace offer match case and whole word, wrap, and replace all as one undo group.
- [x] Auto-indent, bracket and quote pairing, block indent and outdent, toggle comment and duplicate line work.
- [x] Soft wrap and line numbers are independent toggles with sensible defaults.
- [x] The statement holding the caret is tinted without copying and splitting a statement over 64 KiB.
- [x] Syntax errors with a position are underlined, move with edits and show their message on hover.
- [x] Autocomplete covers relations, aliases, columns, schemas, CTEs, functions and keywords from the Catalog cache and the Dialect, capped at 200 items.
- [x] A statement over 64 KiB is parsed off the UI thread for its scope.
- [x] ⌘↵ runs the statement at the caret or the selection, and ⇧⌘↵ runs the script, both remappable.
- [x] An Editor tab is bound to a Connection and an optional server database, shown in the tab and the breadcrumb, and a database can be chosen from the breadcrumb while idle.
- [x] Switching a tab's Connection switches its syntax and drops its database binding.
- [x] Every settled statement is recorded in History with its Connection, database, timing and outcome, except statements that may hold a secret.
- [x] History is searchable, filterable by Connection, limited to 200 results per search, and kept to the newest 100,000 entries.
- [x] A History entry reopens on its original Connection and database, and can be inserted, copied or cleared with confirmation.
- [x] Snippets are named, live in folders, are global or scoped to one Connection, and can be inserted, opened, edited and deleted with confirmation.
- [x] A snippet name is suggested from the first content line when saving from the editor.
- [x] When the local database cannot be opened, History and Snippets run in memory with a visible warning.
- [x] Statements edited through MCP appear in the editor and are marked like Agent edits.

## Out of Scope

- Language server features such as go-to-definition, rename refactoring or diagnostics beyond syntax errors.
- Formatting rules beyond the SQL formatter.
- Sharing Snippets between users or machines.
- Editing the same tab from two windows.

## Further Notes

This task records retroactively the work that built the area as it exists today. The design document mentioned tree-sitter for statement detection as well; the code detects statements with the lexer and uses the grammar for the completion scope.
