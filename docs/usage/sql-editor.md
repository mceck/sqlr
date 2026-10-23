# The SQL editor

The editor is built to stay responsive with large files. It only lays out the lines you can see, so a 10 MB script scrolls smoothly and typing feels immediate.

## Running SQL

| You want to run | Press |
|---|---|
| The statement your cursor is in, or the selected text | ⌘↵ |
| Every statement in the editor, in order | ⇧⌘↵ |
| Nothing, you changed your mind | Esc, or the Stop button |

The statement under the cursor is lightly tinted, so you can see what ⌘↵ is about to run. Statements are split the way the database's own syntax does it, so semicolons inside strings, comments and function bodies do not cut a statement in half.

When a script runs, every statement that returns rows gets its own result tab. Statements that do not return rows (an `UPDATE`, a `CREATE TABLE`) report in the Messages tab: what ran, how long it took, how many rows were affected, and any notices from the server.

If a statement in a script fails, the script stops at that statement by default. To keep going instead, turn off **Stop a script at its first error** in Settings under General.

If a statement has a syntax error and the server reports where, the error is underlined in the editor.

## Autocomplete

Suggestions open as you type, and Ctrl+Space opens them on demand. They know your schema:

- after `FROM` and `JOIN`, you get tables and views
- after `alias.`, you get the columns of that table
- everywhere else, you get keywords and the built-in functions of your database

Enter accepts a suggestion, and Tab does too if you switch on **Insert completion on Tab**. In Settings under Editor you can change the delay, turn off automatic opening and choose the case of inserted keywords.

## Formatting

⌥⌘L reformats the SQL in the editor, or just the selection. The formatter is configurable in Settings under Editor: keyword case, indentation, maximum line length, commas at the start of a line, a new line before `AND` and `OR`, and a blank line between statements.

## Editing helpers

| Action | Shortcut |
|---|---|
| Find, find next, find previous | ⌘F, ⌘G, ⇧⌘G |
| Find and replace | ⌥⌘F |
| Toggle line comment | ⌘/ |
| Duplicate line or selection | ⌘D |
| Undo, redo | ⌘Z, ⇧⌘Z |
| Indent, outdent | Tab, ⇧Tab |

The editor supports several carets at once, automatic indentation and bracket pairing. Soft wrap and line numbers can be switched on in Settings under Editor, and soft wrap also has a command in the palette.

## Tabs

⌘T opens a new query tab. ⌘N opens one on the connection you are using right now. ⌘W closes a tab, and Ctrl+Tab and Ctrl+⇧Tab move between tabs.

Each tab remembers its connection and database, shown in the breadcrumb under the tab strip. Open objects, collections and Redis databases are tabs too.

## History

Every statement you run is recorded with the connection, the time, how long it took and whether it worked. Open the History panel from the View menu to search it. From there you can insert a past query into the editor, copy it, or reopen it on its original connection and database. The same entries show up in the command palette, so ⌘⇧P followed by a few words from an old query usually finds it.

## Snippets

Save a query you use often as a snippet with **Save as snippet** from the Query menu or the palette. A snippet can belong to one connection or be available everywhere, and it can live in a folder. Find snippets again through the palette.

## Explain

**Explain** and **Explain Analyze** (⌥⌘E and ⌥⇧⌘E) show how the database plans to run, or did run, your query, as a tree with cost, rows and time per step. See [Schema tools](schema-tools.md#query-plans).
