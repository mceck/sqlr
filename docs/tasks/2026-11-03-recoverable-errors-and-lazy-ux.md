---
status: in-progress
---

# Recover automatically from warnings and errors the app can fix itself

## Specs

- [Query execution](../specs/query-execution.md)
- [Results and editing](../specs/results-and-editing.md)
- [Explorer and objects](../specs/explorer-and-objects.md)

## Problem Statement

Several situations stop the user with a warning or an error that the app could resolve on its own. After a failed statement in an open transaction on PostgreSQL the Session is stuck in Failed: the next statement returns the raw "current transaction is aborted" error until the user presses Rollback, and Apply changes is refused too. Closing a tab, disconnecting or quitting asks "Commit or Roll back?" even for a transaction that wrote nothing or has already failed. Sorting or filtering a Result with pending edits is refused with a toast and the click does nothing. A failing server-side filter replaces the grid with an error panel, and the filter bar disappears, so the filter cannot be corrected. After idle time the first statement fails with a connection error and has to be re-run by hand. The Redis, Administration and object tabs say "Not connected" instead of connecting. DDL run from the editor leaves the explorer tree stale. Finally, when a schema or database is opened, its folders flash in the tree and vanish once they turn out to be empty.

## Solution

The app absorbs these cases and tells the user only what is useful:

- A statement that fails inside an open transaction is undone on its own and the transaction stays open.
- A transaction that has written nothing, or that is already Failed, ends silently when it would otherwise prompt.
- An action blocked by an open transaction with writes offers "End the transaction and continue".
- Sort and filter with pending edits ask "Discard N pending changes and …?" instead of refusing.
- A failed server-side filter or sort keeps the previous Result visible and shows the error under the filter bar.
- A lost connection is recovered on the next statement: a read is re-run once, a write fails and the next run reconnects.
- Tabs connect their Connection on their own, and a new query tab gets a default Connection.
- A successful DDL refreshes the already loaded tree nodes.
- Opening a schema or database shows a spinner on it until the folders to show are known, then shows them at once.

## User Stories

1. As a user in a Manual or Smart transaction on PostgreSQL, I want a failing statement to be undone on its own, so that I can fix the typo and keep going without losing earlier work.
2. As a user, I want the transaction to stay open after a failed statement, so that the status bar and toolbar still reflect my work.
3. As a user, I want the raw "current transaction is aborted" error never to appear for my own mistakes, so that I am not confused about what failed.
4. As a user, I want Apply changes to work after a failed statement, so that a typo in the editor does not block my edits.
5. As a user whose transaction ended up Failed for another reason, I want it rolled back silently when I close the tab, disconnect or quit, so that I am not asked to "Commit" something that can only roll back.
6. As a user who only read data in a Manual transaction, I want closing the tab, disconnecting or quitting not to ask anything, so that read-only work never nags me.
7. As a user whose idle transaction wrote nothing, I want it rolled back automatically when the idle timeout expires, so that locks and snapshots are released without a prompt.
8. As a user whose idle transaction did write, I want to keep the Commit, Roll back and Keep open prompt, so that I never lose work.
9. As a user who switches a tab to another Connection while its transaction has writes, I want a dialog offering Commit, Roll back and Cancel that then completes the switch, so that I do not have to end the transaction by hand and repeat the action.
10. As an Agent, I want the explicit "commit or roll back first" error to stay, so that no unattended decision is taken for me.
11. As a user with pending edits, I want clicking a column header to ask whether to discard them and sort, so that the click is never silently ignored.
12. As a user with pending edits, I want applying a server-side filter to ask the same, so that sort and filter behave alike.
13. As a user who types a bad WHERE filter, I want to keep seeing the previous Result with the server's error under the filter bar, so that I can fix the filter.
14. As a user who types a bad filter, I want the filter bar to stay editable and clearable, so that I can get back to the unfiltered data in one click.
15. As a user returning to a tab after an idle period, I want a read statement to run even if the connection dropped, so that I do not have to press Run twice.
16. As a user whose write statement hit a dropped connection, I want it to fail without being repeated, so that a write never runs twice without my say-so.
17. As a user with an idle Session, I want the app to notice a dead connection before I run something, so that the first Run is on a live connection.
18. As a user opening a Redis, Administration or object tab, I want the Connection to connect on its own, so that I do not have to go to the explorer first.
19. As a user opening a new query tab, I want it to use the active Connection or the last tab's, so that I can run at once.
20. As a user with several Connections and none active, I want to be asked to choose one when I run a blank tab, so that Run never silently does nothing.
21. As a user who runs `CREATE TABLE` or `DROP TABLE` in the editor, I want the explorer tree to show the change, so that I do not have to refresh it.
22. As a user opening a schema or database, I want a spinner on it while its folders load, so that the tree does not flicker.
23. As a user with "Show empty folders" off, I want only the non-empty folders to appear, all at once, so that the tree is stable.
24. As a user whose folder count fails, I want that folder to be shown with the error when opened, so that a failure never hides objects.

## Implementation Decisions

- **Statement savepoint.** In an open transaction in Manual or Smart mode, on a Dialect that declares that an error aborts the transaction, the engine runs each user statement inside a Statement savepoint: it sets the savepoint before the statement, releases it on success and rolls back to it on failure. A typed transaction verb never gets one. The savepoint is not listed in the toolbar's Rollback to ▾ and does not count as a user savepoint, so it neither starts a transaction nor marks the transaction as having written. A failed statement therefore leaves the phase Open. Dialects where an error does not abort the transaction (MySQL, MariaDB, SQLite, Oracle) need no Statement savepoint. The transaction's first statement, which opens it, needs none either: if it fails, the transaction is rolled back and the phase goes back to Idle.
- **Failed phase is a fallback.** Failed remains for the case where rolling back to the Statement savepoint fails too, and for a mid-fetch error or cancel inside a user transaction. The engine updates the transaction phase on a failed or cancelled fetch (today only the execute path does), either by wrapping the streaming statement in a Statement savepoint as well or by probing the server's transaction status.
- **Failed handling.** Whenever a prompt would offer Commit or Roll back for a Failed transaction (closing a tab, disconnecting, reconnecting, quitting, idle timeout, Auto-commit switch), the app rolls it back silently. Apply changes on a Failed Session rolls back first and applies in a new transaction, with a message in the Output.
- **Clean transaction.** A transaction that is open, has written nothing and has no user savepoint ends with a rollback and no prompt when the user closes a tab, disconnects, reconnects or quits, and when the idle timeout expires. The idle case shows a toast "Transaction on `<name>` closed (idle, no changes)". With writes the existing prompts remain unchanged.
- **Actions blocked by a transaction.** The "commit or roll back first" refusals (a tab switching Connection, transfer, administration, object editor Apply, a statement for another database) reuse the end-transaction dialog with a button "End the transaction and continue", which runs the original action once the transaction ended. A failed commit abandons the action. Clean and Failed transactions are ended silently first. The MCP tools keep their explicit error.
- **Sort and filter with pending edits.** The click opens "Discard N pending changes and sort?" (or "… and filter?"), the same dialog Refresh uses. Confirming discards the edits and runs the server query.
- **Failed server filter or sort.** When a server-side rerun started from the filter bar or a header click fails, the Result tab keeps its previous Result and view, the filter bar shows the server's error under it and stays editable, and the failed filter or sort is not recorded as applied. A "Remove filter" action stays available. A failed rerun that comes from the editor (Run) keeps its current behaviour.
- **Connection recovery.** An execute that fails with a Connection error outside a transaction reconnects the Session. If the statement class is read (a SELECT, SHOW or EXPLAIN without analyze), it is re-run once and the Messages pane says it reconnected. If it writes, the Result fails with the error and the next run uses the new connection. A statement inside a transaction is not retried, because the transaction was lost (existing message). The engine pings Sessions idle for more than a minute that hold no transaction and reconnects those whose connection is dead; the ping uses a cheap server round trip and shares the idle-check tick of the UI.
- **Auto-connect.** Redis, Administration, object and document tabs connect their Connection when they are shown and it is not connected, with the Connecting state, and resume after the secrets prompt as a SQL tab does. A new query tab defaults to the Connection active in the explorer, else the previous tab's; with several Connections and none active the picker opens. Run on a tab without a Connection opens the picker instead of a toast.
- **DDL refresh.** After a successful statement classified DDL, the engine refreshes the already loaded explorer nodes of that Connection and database, in addition to invalidating the Catalog.
- **Folder flicker.** While the folders to preload for an opened schema or database have unknown counts, the parent Node shows its loading spinner and none of those folders is drawn. When all counts are known, the non-empty folders appear together. `tables` is always shown and does not wait. A folder whose count failed is shown, with the error on opening. The empty-folder rule in the snapshot and in the tree widget changes from "a folder still loading stays" to "a folder still loading is held back with its parent".
- **Glossary.** The terms Statement savepoint and Clean transaction are in the glossary.

## Testing Decisions

- **Engine seam** (`FakeDriver` through the engine's execute and transaction API): Statement savepoint on error-aborts Dialects (success, failure, first statement, typed verbs, Dialects without error abort), Clean and Failed transaction handling in the end-transaction decision, mid-fetch failure phase, connection error retry for read versus write, Failed apply. Prior art: the transaction tests in the engine and the testkit's `FakeDriver`.
- **Real PostgreSQL** (containers profile): one integration test that runs `SELECT 1/0` inside a Manual transaction and then a valid statement, asserting the transaction stays open and earlier writes survive. Prior art: existing PostgreSQL driver and engine tests that use per-test databases.
- **UI seam** (`egui_kittest`): the pending-edits dialogs for sort and filter, the failed filter state (previous Result and inline error), the folder holding while counts load, and the end-transaction dialog with "End and continue". Prior art: `crates/sqlr-ui/tests/tree.rs` (`tree_hides_loaded_empty_folders_unless_asked`) and the app's pending and transaction dialog tests.
- **App seam**: tab auto-connect and default Connection, DDL refresh.
- Tests are written first and fail before the change (TDD).

## Acceptance Criteria

- [x] A failing statement in a Manual or Smart transaction on PostgreSQL leaves the transaction Open with earlier writes intact, and the next statement runs.
- [x] No user-visible "current transaction is aborted" error follows an ordinary failed statement.
- [x] A transaction that ends up Failed is rolled back silently by close tab, disconnect, reconnect, quit, idle timeout and the Auto-commit switch.
- [x] Apply changes on a Failed Session works.
- [x] A fetch failure or cancel inside a user transaction updates the transaction phase.
- [x] A transaction without writes ends silently on close tab, disconnect, reconnect and quit, and on idle timeout with a toast; with writes the prompts are unchanged.
- [x] Switching a tab's Connection with an open transaction offers "End the transaction and continue"; MCP tools still return the error.
- [x] Sort and filter with pending edits ask to discard them and then run.
- [x] A failed filter or sort keeps the previous Result, shows the error under an editable filter bar and is not recorded as applied.
- [x] A read that fails with a Connection error outside a transaction is re-run once after reconnecting; a write fails and the next run reconnects.
- [ ] Idle Sessions without a transaction are pinged and reconnected when dead.
- [x] Redis, Administration, object and document tabs connect on their own; a new query tab gets a default Connection.
- [x] A successful DDL from the editor refreshes the loaded explorer nodes.
- [x] Opening a schema or database shows a spinner on it and then the non-empty folders at once, with no folder flashing.
- [ ] The three specs and the glossary describe the new behaviour.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt` and `make test-fast` pass, and the PostgreSQL integration test passes against a real server.

## Out of Scope

- Applying the WHERE filter on blur.
- Re-run and reconnect buttons in the generic error panel, and the "stopped early" Re-run.
- Persistent banners for errors with actions (toasts are unchanged).
- Automatic Recompute of a stale plan.
- Retrying a failed Catalog load after a reconnect.
- "End the transaction and continue" for transfer, administration, the object editor's Apply and a statement for another database; only a tab's Connection switch uses it. The Connection picker does not open by itself for a blank tab with no default.
- Changing the page size of a Result that is already loaded.
- Keeping pending edits across a Refresh; the Refresh dialog stays.
- Restoring a transaction lost with the connection.

## Further Notes

- Source: SQLR-4, from a review of lazy warnings and recoverable errors in the app. The investigation found 22 cases; the ones above were chosen with the user.
- Explain Analyze already asks for confirmation only for statements that write, so it needs no change.
- The spec for the explorer says today that a folder still loading stays in the tree; this task reverses that rule.
- The explorer spec also says DDL run in the editor only invalidates the Catalog; this task reverses that too.
