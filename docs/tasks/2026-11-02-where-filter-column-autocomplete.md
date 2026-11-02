---
status: done
---

# Column completion in the Result grid's WHERE field

## Specs

- [Results and editing](../specs/results-and-editing.md)

## Problem Statement

The server filter above a Result grid takes a SQL `WHERE` condition, but the user has to remember and type the Result's column names. Long names, names with spaces or reserved words, and names whose case matters are easy to mistype, and a typo only shows up as a server error after the query reruns.

## Solution

While the user types in the WHERE field, a popup lists the columns of the Result whose names match the word at the caret. ↑ and ↓ move the selection, ↵ or Tab inserts the selected name, Esc closes the list, and clicking a name inserts it. The inserted name is quoted with the Dialect's identifier rules when needed. The list never opens inside a string literal, a quoted identifier or a comment, and Ctrl+Space (the Complete shortcut from the Keymap) opens it even before a word is typed.

## User Stories

1. As a user filtering a Result, I want the columns of the Result offered while I type, so that I do not have to remember their names.
2. As a user filtering a Result, I want the matching to ignore case and to accept a match inside the name, so that `na` finds `name` and `id` finds `order_id`.
3. As a user, I want the current word replaced by the chosen column, so that completing does not leave the typed prefix behind.
4. As a user, I want ↑ and ↓ to move through the list and ↵ or Tab to insert, so that I can complete without leaving the keyboard.
5. As a user, I want Esc to close the list without changing the condition, so that I can ignore it.
6. As a user, I want to click a column in the list, so that the mouse works too.
7. As a user, I want ↵ to run the filter when the list is closed, so that the field keeps its existing behavior.
8. As a user, I want a column whose name needs quotes (spaces, mixed case, reserved words) inserted quoted for my database, so that the condition is valid.
9. As a user, I want no list while I type inside a string, a quoted identifier or a comment, so that the popup does not get in the way of a literal.
10. As a user, I want Ctrl+Space to show the columns before I type a prefix, so that I can explore the Result and pick a name.
11. As a user with duplicate column names in the Result, I want one entry per name, so that the list is not repetitive.
12. As a user, I want the popup to close when the field loses focus or the filter runs, so that it never covers the grid when I am not filtering.

## Implementation Decisions

- The completion list contains the Result's own columns, as shown by the grid's headers. It does not read the Catalog: the server filter wraps the original statement as a subquery, so only the Result's output names are valid.
- The word at the caret is lexed with the Dialect's `SqlSyntax`, clipped to the condition text. An open string, quoted identifier or comment suppresses the list, as does a caret at the end of a closing quoted identifier.
- Matching is case-insensitive: prefix matches come first, then substring matches, each in the Result's column order. Exact duplicate names are listed once.
- The inserted identifier is `SqlSyntax::quote_if_needed`, so dialects keep their own quote characters (MySQL backticks, SQL Server brackets) and reserved words are quoted.
- The popup opens when the field changes with a non-empty word at the caret, or with Ctrl+Space (the Complete action in the Keymap), which also opens it with an empty word.
- Grid state keeps the popup's open flag, explicit flag, selected row and the popup rectangle; the matching items are recomputed from the field text and caret each frame, so no stale list survives a text change.
- Keys are consumed before the single-line field sees them: ↑/↓ move, ↵/Tab insert, Esc closes. With the popup closed, ↵ keeps its meaning of running the server filter.
- The popup is an `egui` Area in the foreground under the field, with the same popup frame and scroll behavior as the editor's completion popup. Clicking a row inserts and returns focus to the field.
- The grid widget stays engine-free: completion uses the rows and Dialect it already receives, and still only queues `GridAction::FilterOnServer`.

## Testing Decisions

- Unit tests in the grid's filter module cover the word at the caret, suppression inside literals/comments, match ordering and deduplication, and identifier quoting.
- `egui_kittest` integration tests in the existing grid editing suite cover typing a prefix and inserting with ↵, choosing a different row with ↓, ↵ running the filter when the list is closed, and no interception inside a string literal.
- The tests use the existing `data_grid` harness and its Result columns; no new seam is introduced.

## Acceptance Criteria

- [x] Typing a column prefix in the WHERE field lists the matching Result columns.
- [x] ↑/↓, ↵, Tab, Esc and clicks behave as described.
- [x] The inserted identifier is quoted for the Dialect when needed.
- [x] The list is suppressed inside strings, quoted identifiers and comments.
- [x] Ctrl+Space opens the list with an empty word too.
- [x] The popup closes on focus loss, on Esc and after insertion, and ↵ runs the filter when it is closed.
- [x] `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings` and the UI tests pass.

## Out of Scope

- Catalog tables, functions and SQL keywords in the WHERE field: the list is the Result's own columns.
- Value suggestions and automatic quoting of literals.
- Completion in other text fields, such as the console or the find bar.

## Further Notes

The field hides itself for Results that no query produced (`GridState::hide_filter`), so completion only applies to SQL Results.
