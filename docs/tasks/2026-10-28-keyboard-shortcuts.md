---
status: in-progress
---

# Keyboard shortcuts: pending changes, refresh, tabs, grid rows and VS Code editor commands

## Specs

- [Settings and workspace](../specs/settings-and-workspace.md)
- [Results and editing](../specs/results-and-editing.md)
- [Explorer and objects](../specs/explorer-and-objects.md)
- [Query editor](../specs/query-editor.md)

## Problem Statement

⌘S applies only an Object editor's Schema change list. With a Changeset pending in a Result grid, on an Object editor's Data or Permissions page, in the Users and roles editor, on a Redis value or in a document's JSON editor, ⌘S does nothing and the user has to reach for the Apply or Save button. Common keyboard work has other gaps too:

- Discarding pending edits needs the mouse.
- Reloading a result, an object, an administration panel or a key list needs the mouse.
- Jumping to a given tab means cycling with Ctrl+Tab.
- A tab closed by mistake is gone: its text, Connection and database have to be rebuilt by hand.
- In the Result grid, "Add row", "Duplicate row" and "Set NULL" exist only in the context menu and the result bar, while "Delete row" has a chord.

The editor already has several carets. Only ⌥-click and ⌥⌘↑/↓ create them, though, and a user coming from VS Code expects other habits too:

- ⌘D selects the next occurrence of a word.
- ⇧⌘L selects every occurrence.
- ⌥↑/⌥↓ moves a line.
- ⇧⌘K deletes a line.

In sqlr, ⌘D duplicates the line, and the other chords do nothing. Renaming an alias across a statement or editing a column list therefore takes several clicks.

## Solution

- **Apply pending changes (⌘S)** applies the pending edits of whatever the active tab shows, exactly as its Apply or Save button would, confirmations included. With nothing pending it does nothing.
- **Discard pending changes** has no default chord. It drops those edits exactly as the surface's Discard, Revert or Cancel button would, and is reachable from the Palette or a user binding.
- **Refresh (F5 and ⌘R)** reloads what the active tab shows and asks first when that would drop pending edits.
- **Go to tab 1–8 (⌘1…⌘8)** and **go to the last tab (⌘9)** focus a tab by its position.
- **Reopen closed tab (⇧⌘T)** brings back the most recently closed tabs of this session, one per press.
- In a focused Result grid: **Add row (⌘I)**, **Duplicate rows (⌘D)** and **Set NULL (⌥⌘N)**.
- In the editor, the sqlr preset adopts VS Code's chords on macOS. Every command works on every caret and undoes as one group.

| Command | Chord |
|---|---|
| Add selection to next occurrence | ⌘D |
| Select all occurrences | ⇧⌘L |
| Undo the last cursor operation | ⌘U |
| Add cursor above, add cursor below | ⌥⌘↑, ⌥⌘↓ (now remappable) |
| Add cursors to line ends | ⇧⌥I |
| Move line up, move line down | ⌥↑, ⌥↓ |
| Copy line up, copy line down | ⇧⌥↑, ⇧⌥↓ (copy line down replaces Duplicate line) |
| Delete line | ⇧⌘K |
| Select line | ⌘L |
| Indent line, outdent line | ⌘], ⌘[ |

Every new action appears in the Shortcuts dialog and the Keymap editor and can be rebound. Apply, Discard, Refresh and Reopen closed tab are also Palette commands.

## User Stories

1. As a user editing cells in a Result grid, I want ⌘S to apply the Changeset, so that I don't have to click Apply in the result bar.
2. As a user on a Connection whose profile asks before writing, I want ⌘S to open the same review dialog as the Apply button, so that the shortcut never bypasses a Safety flag.
3. As a user typing in a cell's inline editor, I want ⌘S to commit the cell first and then apply, so that the value I just typed is included.
4. As a user whose typed cell value is invalid, I want ⌘S to leave the editor open with its message and apply nothing, so that I can fix the value.
5. As a user running a script with several Results, I want ⌘S to apply the Changeset of the Result I am looking at, so that edits in other Results stay untouched.
6. As a user whose Changeset is already being applied, I want ⌘S to do nothing, so that the Changeset is not applied twice.
7. As a user of an Object editor, I want ⌘S on the Data page to apply the row Changeset, so that it behaves like that page's Apply.
8. As a user of an Object editor, I want ⌘S on the Permissions page to apply the grants, so that it behaves like "Apply grants".
9. As a user of an Object editor, I want ⌘S on any other page to apply the Schema change list, as it does today.
10. As an administrator editing a role in the Users and roles editor, I want ⌘S to apply the role changes, so that it behaves like that editor's Apply.
11. As a Redis user editing a string value, I want ⌘S to save the value, so that it behaves like the Save button.
12. As a Redis user editing a collection element inline, I want ⌘S to save the element, so that it behaves like the Save button or Enter.
13. As a MongoDB or Elasticsearch user editing or inserting a document in the JSON view, I want ⌘S to save it, so that it behaves like the Save button, validation included.
14. As a user with nothing pending, I want ⌘S to do nothing, so that a reflexive save never has a side effect.
15. As a user with a modal dialog open, I want ⌘S not to apply the tab behind the dialog, so that I never write by accident while reading a prompt.
16. As a user, I want a Discard pending changes command, so that I can drop pending edits from the keyboard through the Palette or my own binding.
17. As a user, I want Discard pending changes to have no default chord, so that a mistyped chord never throws work away.
18. As a user, I want Discard to drop exactly what the active surface's Discard, Revert or Cancel button drops, so that its effect is predictable.
19. As a user looking at a Result, I want F5 or ⌘R to run that Result's statement again with its server sort and filter, so that I see fresh data.
20. As a user with a pending Changeset on that Result, I want Refresh to ask before discarding it, so that I don't lose edits.
21. As a user of an Object editor, I want Refresh to reload the object, asking first when edits are pending, as its Refresh button does.
22. As an administrator, I want Refresh to reload the active Users, Sessions or Server panel, asking first when the Users editor has pending edits.
23. As a Redis user, I want Refresh to reload the key list and the open value, so that I see changes made elsewhere.
24. As a Redis user with an unsaved value edit, I want Refresh to leave that edit alone while still refreshing the key list.
25. As a MongoDB or Elasticsearch user, I want Refresh to run the tab's last statements again, so that I see fresh documents.
26. As a user of a DDL viewer, I want Refresh to load the DDL again.
27. As a user, I want Refresh to do nothing while the tab's query is running, so that I don't start a second run.
28. As a user with many tabs, I want ⌘1…⌘8 to focus the first to eighth tab, so that I can jump directly.
29. As a user, I want ⌘9 to focus the last tab whatever the count, as browsers do.
30. As a user, I want "tab N" to follow the same order that Ctrl+Tab cycles through, so that the two agree when tabs are split into groups.
31. As a user who closed a tab by mistake, I want ⇧⌘T to reopen it with its title, Connection, database and editor text, so that I can continue.
32. As a user who closed several tabs, I want each ⇧⌘T to reopen the next most recently closed one.
33. As a user, I want a reopened Object editor, DDL viewer, Redis tab, document tab, administration panel or ER diagram to open on the same object, Connection and database, so that every tab kind can be recovered.
34. As a user, I want a reopened tab not to run anything on its own, so that reopening never touches the database.
35. As a user whose Connection was deleted after closing the tab, I want ⇧⌘T to say so and skip that tab, so that it doesn't fail silently.
36. As a user editing a table in the Result grid, I want ⌘I to add a row, as "Add row" does.
37. As a user, I want ⌘D in the grid to duplicate the selected rows, as "Duplicate row" does, while ⌘D in the SQL editor keeps its own editor meaning.
38. As a user, I want ⌥⌘N in the grid to set the selected writable, nullable cells to NULL, as "Set NULL" does.
39. As a user of a read-only Result, I want those chords to do nothing.
40. As a user, I want the grid's context menu to show the chords of Add row, Duplicate row and Set NULL, so that I can learn them.
41. As a user, I want every new action in the Shortcuts dialog and the Keymap editor, so that I can find and rebind it.
42. As a user who had rebound "Apply the object editor's changes", I want my binding to keep working after the action becomes generic.
43. As a user of the VS Code preset, I want the new chords not to conflict with that preset's bindings.
44. As a user, I want ⌘D with no selection to select the word under the caret, so that I can start a multi-selection from where I am.
45. As a user, I want each further ⌘D to add a selection at the next occurrence of the selected text, so that I can edit several occurrences at once.
46. As a user, I want ⌘D to wrap around to the top of the text, so that occurrences above the caret are reachable.
47. As a user, I want ⌘D to skip occurrences already selected, and to do nothing once all are selected.
48. As a user who started from a word, I want ⌘D to match whole words with the same case, so that selecting `id` does not pick `orders_id` or `ID`.
49. As a user who started from a selection, I want ⌘D to follow the find bar's Match case and Whole word options, as VS Code does.
50. As a user, I want the newest selection to become the primary caret and scroll into view, so that I see what I added.
51. As a user, I want ⇧⌘L to select every occurrence at once.
52. As a user, I want ⌘U to undo the last caret operation without undoing text, so that one ⌘D too many is easy to take back.
53. As a user, I want ⌥⌘↑ and ⌥⌘↓ to stay as they are but be listed and remappable in the Keymap editor.
54. As a user, I want ⇧⌥I to put a caret at the end of every line of my selection, so that I can append to many lines.
55. As a user, I want ⌥↑ and ⌥↓ to move the lines under my selections, with the selections following them.
56. As a user, I want ⇧⌥↓ and ⇧⌥↑ to copy those lines below or above, as in VS Code.
57. As a user, I want ⇧⌘K to delete those lines.
58. As a user, I want ⌘L to select the current line and, pressed again, extend to the next one.
59. As a user, I want ⌘] and ⌘[ to indent and outdent the lines under the caret whatever the selection.
60. As a user, I want each of these commands to undo in one ⌘Z.
61. As a user, I want them to work in every editor that uses the editor widget: SQL, Mongo shell, Elasticsearch console and JSON.
62. As a user who had rebound Duplicate line, I want my binding to keep working on Copy line down.

## Implementation Decisions

### Pending changes, refresh, tabs and grid

- **Apply action.** `ActionId::ApplyObject` becomes a generic `Apply` action labelled "Apply pending changes", Global scope, bound to ⌘S. Its `keymap.toml` id becomes `apply_changes`. `ActionId::from_id` also accepts the legacy id `apply_object`, so stored overrides keep working; writing always uses the new id. Add a test where a stored `apply_object` binding loads as `Apply`.
- **Apply routing.** One app-level method resolves the active tab's apply target and calls the same entry point as that surface's button, so confirmations and refusals are shared:
  - **Editor tab:** apply the Changeset of `shown_query()`. Use the `changes_action(Apply)` path, which opens the review dialog when the profile has Confirm writes. Skip when that Result's Changeset is empty, an apply is running for it, or the shown pane is not a Result.
  - **Object editor:**
    - Data page shown: the Data page's Result, same path as an Editor tab.
    - Permissions page shown: the "Apply grants" path.
    - Any other page: the existing `apply_object_tab`.
  - **Users and roles editor:** `AdminCommand::ApplyRole` when the editor has pending edits.
  - **Redis tab:**
    - An inline collection element editor is open: save it, as Enter does.
    - Otherwise, the string value differs from its loaded text: save it.
    - Expose a small public query on the key-value view state for "has an unsaved edit" and a way to request its save, instead of reading private fields.
  - **Document tab:** the JSON view is editing or inserting: request its Save, which keeps its validation and the Confirm writes path.
  - **DDL viewer, ER diagram and other tabs:** nothing.
- **Inline cell editor.** When the active Result grid has an inline cell editor open, Apply first commits it as Enter would. If the value is invalid, the editor stays open with its message and nothing is applied. An unsaved edit in the Value panel is not staged; Apply behaves like the result bar's Apply button.
- **Discard action.** A new Global `Discard` action, "Discard pending changes", with no binding in either preset. It uses the same target resolution as Apply and calls the surface's own discard:
  - Result grid: `ChangesAction::Discard`.
  - Object editor:
    - DDL pages: Revert.
    - Data page: the grid discard.
    - Permissions page: "Revert grants".
  - Users and roles editor: Revert.
  - Redis: cancel the inline element editor, or revert the string value.
  - Document JSON view: Cancel.

  Discard asks for no confirmation, like the buttons. A chord is never bound by default.
- **Modal dialogs.** Apply, Discard, Refresh and Reopen closed tab do nothing while any modal dialog is open. Today the action filter in `dispatch_actions` only blocks while Preferences or the Palette is open. Add one check for "a modal is open" that covers sqlr's own dialogs (`theme::Dialog`, confirmation and discard prompts, the review dialog), and use it for these four actions. ⌘S triggering a dialog's own Save is out of scope.
- **Refresh action.** A new Global `Refresh` action, "Refresh", bound to F5 and ⌘R in the sqlr preset.
  - **Editor tab:** re-run the shown Result with its server sort and filter, like the private `rerun_result` used after an apply. A pending Changeset on that Result first gets a prompt: "Discard N pending changes and refresh?" with Refresh and Cancel. The prompt reuses the Object editor's discard prompt pattern. With no Result shown, or while the tab is running, Refresh does nothing. It never re-runs the editor text.
  - **Object editor:** `ObjectEditorAction::Refresh`, which already asks when edits are pending.
  - **Administration:**
    - Users: the Reload path, which already asks.
    - Sessions: its refresh.
    - Server: reload the active pane.
  - **Redis:** rescan the key list. Also reload the open value, unless it has an unsaved edit or an inline element editor is open.
  - **Document tab:** re-run the tab's last statements (`DocCommand::Rerun`). Skip while the JSON view is editing or the tab is running.
  - **DDL viewer:** `DdlAction::Reload`.
  - **ER diagram:** nothing.
- **Go to tab.** Nine Global actions in the Navigation group:
  - "Go to tab 1" … "Go to tab 8", bound to ⌘1…⌘8.
  - "Go to the last tab", bound to ⌘9.

  Positions follow `dock.iter_all_tabs()`, the order Next tab uses. A position past the last tab does nothing. These actions are not added to the native macOS menu, because its chord mapping does not handle digits.
- **Reopen closed tab.** A new Global action "Reopen closed tab" in the Navigation group, bound to ⇧⌘T.
  - **The stack.** `close_tab_now` pushes a recipe onto an in-memory stack capped at 20 entries. The stack lasts for the session only and is never persisted. A recipe holds only what is needed to open the tab again; it never holds secrets, Result rows or pending edits:
    - Editor tab: title, Connection id, database, editor text.
    - Object editor: Connection id, object reference, open page.
    - DDL viewer: Connection id and node.
    - Redis tab: Connection id, database, selected key.
    - Document tab: title, Connection id, database, text.
    - Administration panel: Connection id and panel.
    - ER diagram: Connection id, database, schema, tables.
  - **Reopening.** Pop the top recipe and open it through the existing open functions (`new_tab`, `push_object_tab`, `open_ddl`, `open_redis`, `open_doc_tab` with run off, `open_admin`, `open_er_diagram`), then focus it. Nothing runs automatically. A disconnected Connection connects the way opening the tab normally does.
  - **Deleted Connection.** If the recipe's Connection no longer exists, show the Toast "The connection of the closed tab no longer exists", drop the recipe and stop. The next press reopens the next recipe.
  - **Text editor tabs.** An Editor tab without a Connection is a valid recipe.
- **Grid scope.** A new Keymap scope, `Grid`, is active while a Result grid has the keyboard. Grid and Editor do not overlap, so the same chord can mean one thing in the SQL editor and another in the grid; Global still overlaps both. `DeleteRows` moves from Editor to Grid scope; its id stays the same. The grid keeps consuming the shared Editor-scope actions Undo, Redo, SelectAll and Copy as today.
- **Grid row chords.** The grid's keyboard handler, which already consumes Undo, Redo, SelectAll and DeleteRows through the Keymap, also consumes three new Grid-scope actions in the Results group:
  - **Add row:** "Add row", ⌘I. It calls `GridState::add_row`.
  - **Duplicate rows:** "Duplicate rows", ⌘D. Move the duplicate logic now inline in the context menu handler into a `GridState` method that both paths call.
  - **Set NULL:** "Set NULL", ⌥⌘N. It calls the existing set-NULL operation on the selection.

  Each chord reports the same `GridAction` as the menu entry. It does nothing on a read-only grid, with no selection where one is needed, or while an inline cell editor is open. The context menu shows the chords through `menu_entry`, like Copy and Delete.
- **Palette.** Add "Apply pending changes", "Discard pending changes", "Refresh" and "Reopen closed tab" to the Palette commands, with their chords from `action_id()`.
- **Presets.** Add the new bindings to the `SQLR` table. The `VSCODE` table changes nothing. `presets_have_no_conflicts` must still pass on macOS and elsewhere. On other platforms ⌘ is Ctrl, so the chords become Ctrl+S, F5, Ctrl+R, Ctrl+1…9, Ctrl+Shift+T, Ctrl+I and Ctrl+Alt+N.
- **Groups.** Apply and Discard move to the General group. `actions!` stays grouped in `GROUPS` order.
- **Terminal.** While the Integrated terminal has focus, global actions stay filtered as today, so ⌘R, ⌘1… and ⇧⌘T reach the shell.

### Editor commands

- **New Editor-scope actions**, all in the Editor group, all bound in the `SQLR` table:
  - Add selection to next occurrence: `Cmd+D`.
  - Select all occurrences: `Cmd+Shift+L`.
  - Undo last cursor operation: `Cmd+U`.
  - Add cursor above, add cursor below: `Cmd+Alt+ArrowUp`, `Cmd+Alt+ArrowDown`.
  - Add cursors to line ends: `Shift+Alt+I`.
  - Move line up, move line down: `Alt+ArrowUp`, `Alt+ArrowDown`.
  - Copy line up: `Shift+Alt+ArrowUp`.
  - Delete line: `Cmd+Shift+K`.
  - Select line: `Cmd+L`.
  - Indent line, outdent line: `Cmd+]`, `Cmd+[`.
- **Copy line down.** The existing `DuplicateLine` keeps its id `duplicate_line`, so stored bindings keep working. It is relabelled "Copy line down" and rebound to `Shift+Alt+ArrowDown`. The `VSCODE` table drops its now-identical override for it.
- **Add cursor above and below.** The editor's input layer resolves both through the Keymap instead of matching the chord in code.
- **Precedence.** Keymap chords take precedence over the editor's built-in key handling. `Alt+ArrowUp` and `Alt+ArrowDown` stop being plain vertical moves.
- **Edit model.** Each command is an `EditCommand` variant on the existing model, so they all apply across the sorted carets, merge overlaps and record one undo group per press, like `DuplicateLine` and `ToggleComment` today.
- **Next occurrence rules.**
  - **Starting point.** With an empty primary selection, the first press selects the word around the primary caret and adds no caret. That word becomes the needle in whole-word, case-sensitive mode. A caret on no word does nothing. With a non-empty primary selection, its text is the needle with the find bar's Match case and Whole word options.
  - **Search.** The search starts after the most recently added selection, wraps around the document and skips ranges already selected. The found range becomes a new selection and the primary caret, and is revealed. It uses the editor's existing line-by-line find, so a needle that spans a line break does nothing.
- **Select all occurrences.** Same needle rules. It selects every match, keeps the match holding the old primary caret as primary, and stops at 10,000 carets.
- **Caret history.** Every caret-adding command pushes the caret set it replaced onto a caret history, capped at 100 entries: add cursor (⌥-click, above, below), next occurrence, all occurrences, line ends. ⌘U pops it. Any text edit or single-caret placement clears it. ⌘U never touches the text or the undo stack.
- **Line commands.** A line command works on the set of lines touched by any selection. A selection that ends at column 0 of a line does not touch that line.
  - **Move.** Moves the block one line and keeps the selections on the moved text. At the top or bottom of the document it does nothing.
  - **Copy down.** Inserts a copy below and moves the selections onto the copy.
  - **Copy up.** Inserts the copy above and keeps the selections on the upper copy.
  - **Delete.** Removes the lines with their line breaks. Each caret lands on the following line, at its column clamped to that line.
  - **Select line.** Selects from the line start to the start of the next line. When the selection already covers whole lines, it extends by one more.
  - **Indent and outdent.** Shift the lines like Tab and Shift+Tab on a multi-line selection, whatever the selection.
  - **Line ends.** Replaces the carets with one empty caret at the end of each touched line. A single-line selection gets one caret at its line end.
- **Line breaks.** `\r\n` terminators stay hidden as today. Copied and moved lines keep the line break style of the document.
- **Not in the preset.** "Go to line" (`⌃G` in VS Code on macOS) is not added. Off macOS ⌃ and ⌘ are the same key, so `Ctrl+G` would conflict with Find next on `Cmd+G`. Two-chord sequences such as `⌘K ⌘D` are not supported by the Keymap.

## Testing Decisions

- **Seam 1, the app harness** (`egui_kittest`, `sqlr-app/tests`). Press the chord and assert what the `FakeDriver` executed or what the UI shows. It covers:
  - Apply on an Editor tab Result. Base it on `edit_preview_apply_flow` in `smoke.rs`.
  - Apply with Confirm writes opening the review dialog.
  - Apply with an inline cell editor open, both valid and invalid.
  - Apply with nothing pending.
  - Apply under a modal.
  - Apply on an Object editor's Data, Permissions and DDL pages. Use the `objects.rs` helpers `edit_data_cell` and `apply_data_changes`.
  - Apply on the Users and roles editor. Base it on `admin.rs` `users_tab_preview_apply`.
  - Apply on a Redis string value and inline element. Base it on `redis.rs`.
  - Apply on a document JSON edit.
  - Discard through a user binding.
  - Refresh on a Result, with and without a pending Changeset, and on an Object editor.
  - Reopen closed tab for an Editor tab, with text and Connection, and for an Object editor.
  - Reopen after the Connection is deleted.
- **Seam 2, app dispatch without UI.** Call `dispatch_action` and assert `active_key()`. Covers go to tab 1…8 and last, and the reopen stack order and cap. Prior art: `global_actions_switch_tabs_and_open_the_requested_tools` in `tabs.rs`.
- **Seam 3, the grid** (`sqlr-ui/tests/grid_edit.rs`). Key presses on a focused editable grid report `AddRow`, `DuplicateRow` and the set-NULL edit, and do nothing on a read-only grid. Prior art: the `DeleteRows` key tests there.
- **Seam 4, the Keymap** (`sqlr-core`). Covers:
  - A Grid-scope and an Editor-scope action on the same chord do not conflict; a Global one conflicts with both.
  - The new ids round-trip.
  - The legacy `apply_object` id loads as `Apply`.
  - Both presets stay conflict-free on macOS and elsewhere, including ⌘D in both the Editor and Grid scopes.
  - `Discard` has no preset binding.
  - A stored `duplicate_line` binding still loads.
- **Seam 5, the editor's edit model** (`sqlr-ui` editor model unit tests). One test per command over a text and a set of carets, asserting the resulting text and carets and that one undo restores both. Prior art: the existing `DuplicateLine` and multi-caret tests in the editor model. Covers:
  - Next occurrence: word start, wrap, skip already selected, case and whole-word modes, multi-line needle.
  - Select all occurrences, including the 10,000 cap.
  - Caret history and ⌘U.
  - Every line command at document edges, with CRLF text and with several carets.
- **Seam 6, the editor widget** (`egui_kittest` in `sqlr-ui/tests`). Pressing ⌘D, ⇧⌘L, ⌥↓ and ⇧⌘K on a focused editor runs the command through the Keymap. A rebound chord works too. Prior art: the editor input tests, including `app_shortcuts_are_left_alone`.

## Acceptance Criteria

- [ ] ⌘S applies the shown Result's Changeset in an Editor tab, through the review dialog when Confirm writes is on.
- [ ] ⌘S commits an open inline cell editor first; an invalid value blocks the apply and keeps the editor open.
- [ ] ⌘S applies the Data page Changeset, the grants, or the Schema change list, depending on the Object editor's page.
- [ ] ⌘S applies the Users and roles editor, saves a Redis string value or inline element, and saves a document JSON edit.
- [ ] ⌘S does nothing with nothing pending, while an apply runs, or while a modal dialog is open.
- [ ] A stored `apply_object` binding keeps working; the action is saved as `apply_changes`.
- [ ] Discard pending changes exists with no default chord and drops the active surface's pending edits like its button.
- [ ] F5 and ⌘R refresh the active tab per the rules above and ask before dropping a pending Changeset.
- [ ] ⌘1…⌘8 focus tabs 1–8 in Next tab order, and ⌘9 focuses the last tab.
- [ ] ⇧⌘T reopens the most recently closed tabs of the session, of every tab kind, without running anything. The stack holds 20 entries.
- [ ] A recipe whose Connection was deleted shows the Toast and is dropped.
- [ ] ⌘I, ⌘D and ⌥⌘N add, duplicate and set NULL in a focused editable grid, and do nothing on a read-only one.
- [ ] The context menu shows those chords.
- [ ] The new actions are listed in the Shortcuts dialog and the Keymap editor. Apply, Discard, Refresh and Reopen closed tab are in the Palette.
- [ ] ⌘D selects the word, then adds the next occurrences with wrap, skipping selected ones, under the case and whole-word rules above.
- [ ] ⇧⌘L selects every occurrence, up to 10,000.
- [ ] ⌘U undoes the last caret operation and leaves the text alone.
- [ ] ⌥⌘↑/↓ come from the Keymap and can be rebound.
- [ ] ⇧⌥I, ⌥↑/↓, ⇧⌥↑/↓, ⇧⌘K, ⌘L, ⌘] and ⌘[ behave as described, on every caret, one undo group each.
- [ ] Duplicate line is now Copy line down on ⇧⌥↓, and old `duplicate_line` bindings still load.
- [ ] The commands work in the SQL, Mongo shell, Elasticsearch console and JSON editors.
- [ ] Both presets are conflict-free on macOS and elsewhere, with ⌘D in both the Editor and Grid scopes.
- [ ] The specs are updated, and the manual's shortcuts page lists the new chords.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` and the focused tests pass.

## Out of Scope

- ⌘S triggering Save inside modal dialogs (connection dialog, Preferences, snippets).
- Staging an unsaved Value panel edit before Apply.
- Refresh in the Explorer. It has no focus tracking, and its Refresh button and menus stay as they are.
- Asking before closing an Editor tab that holds a pending Changeset. Today that is not asked, and this task does not change it.
- Persisting closed tabs across restarts.
- Adding go-to-tab entries to the native macOS menu.
- A "Set to DEFAULT" grid operation.
- Go to line: off macOS `Ctrl+G` is Find next.
- Two-chord sequences, including "skip this occurrence" (`⌘K ⌘D`).
- Insert line below or above (`⌘↵`, `⇧⌘↵`): those chords run statements.
- Expand and shrink selection, block comments, and column (box) selection by drag.

## Further Notes

- No dependency on other open tasks.
- Glossary terms: Changeset for Result edits and Schema change list for Object editor edits. "Pending changes" appears only in user-facing labels.
- ⌘D changes meaning in the editor and gains one in the grid in the same change, so a build never has two meanings for it in one scope.
