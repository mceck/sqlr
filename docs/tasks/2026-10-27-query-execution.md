---
status: done
---

# Query execution, transactions and Connection profiles

## Specs

- [Query execution](../specs/query-execution.md)
- [Architecture](../specs/architecture.md)
- [Connections](../specs/connections.md)
- [Results and editing](../specs/results-and-editing.md)
- [Settings and workspace](../specs/settings-and-workspace.md)

## Problem Statement

Running a statement looks simple until it is not: a long query in one tab blocks every other tab, a huge result eats all memory, Stop leaves the query running on the server, and nobody can tell whether a transaction is still open. On production databases a mistyped `DELETE` without `WHERE`, or a write in the wrong window, costs real data. Users juggling development, test and production servers need each one to behave with the right amount of caution without reconfiguring every time.

## Solution

Each Editor tab runs on its own Session, so tabs never block each other or the explorer. Results arrive in pages of 500 rows, fetch more on scroll, fetch all with a cap, and stay within memory budgets. Stop cancels on the server where the database allows it, and every in-flight Operation can be cancelled from the Operations panel. Scripts run statement by statement with stop or continue on error. Connection profiles (Development, Test, Production, or the user's own) set the commit mode (Auto, Manual, Smart), read-only mode, confirmations and an idle transaction timeout; the toolbar and status bar always show the real transaction state, and the app asks before closing, disconnecting or quitting with an open transaction.

## User Stories

1. As a developer, I want each Editor tab to have its own Session, so that a long query in one tab does not block another.
2. As a developer, I want the explorer and autocomplete to keep working while my query runs, so that I can explore meanwhile.
3. As a user on a server with few connections, I want a setting to share one Session per Connection, so that sqlr uses fewer connections.
4. As a user, I want to run the statement at the caret, the selection or the whole script, so that I control what runs.
5. As a user, I want to see the running state, elapsed time and rows so far, so that I know a query is progressing.
6. As a user, I want a large table to open with its first 500 rows, so that it opens instantly.
7. As a user, I want more rows to load as I scroll, so that I can browse naturally.
8. As a user, I want to change how many rows each fetch brings (100, 500, 1000, 5000 or custom), so that I can tune paging.
9. As a user, I want Fetch all with progress and Cancel, capped at a configurable row count, so that I can load everything safely.
10. As a user, I want results to stop loading at a memory budget with a clear warning, so that the app never runs out of memory.
11. As a user, I want closing a result to free its memory at once, so that I can reclaim memory.
12. As a user, I want to sort and filter a complete result locally, so that it is instant.
13. As a user, I want to sort or filter a partial result on the server, so that the order covers every row and not only the loaded ones.
14. As a user, I want Stop to cancel the query on the server, so that the database stops working too.
15. As a user, I want a cancelled fetch to keep the rows already loaded, so that I do not lose what I have.
16. As a user, I want a script to run statement by statement with a result tab per query and a line per statement in Output, so that I can follow it.
17. As a user, I want a script to stop at the first error by default, or continue when I choose, so that I control failures.
18. As a user, I want the Messages pane to list each statement's outcome, notices and transaction notes, so that I know what happened.
19. As a user of a test database, I want Smart mode, so that reads stay auto-committed and my first write opens a transaction I must commit.
20. As a user of a production database, I want Manual mode, so that nothing is committed until I say so.
21. As a user, I want to switch the commit mode of the current Session from the toolbar, so that I can change it without editing the profile.
22. As a user, I want Commit, Rollback, savepoints and Rollback to, so that I can control the transaction.
23. As a user, I want the status bar to show how many statements and since when the transaction is open, so that I do not forget it.
24. As a user, I want the explorer to mark Connections with an open transaction, so that I see them at a glance.
25. As a PostgreSQL user, I want to be told that only Rollback is possible after a failed statement, so that I do not wonder why commits fail.
26. As a MySQL user, I want to be told that a DDL statement committed my transaction, so that I am not surprised.
27. As a user, I want to be asked to commit or roll back before closing a tab, disconnecting or quitting with an open transaction, so that I never lose or leave changes by accident.
28. As a user on a dropped network, I want a disconnect to go through 5 s after I answered, so that a dead server cannot trap me.
29. As a user, I want to be reminded of an idle transaction after the profile's timeout, so that I do not hold locks for hours.
30. As a user, I want read-only mode enforced by the server and by sqlr, so that a write cannot slip through.
31. As a user, I want to switch read-only on for one Session from the status bar, so that I can protect a risky session.
32. As a user, I want built-in Development, Test and Production profiles with sensible defaults, so that I am protected without configuring.
33. As a user, I want to create my own profiles and edit or reset the built-ins, so that the policies fit my team.
34. As a user, I want the profile color on the toolbar, tabs and explorer, so that I always see when I am on production.
35. As a user, I want to confirm destructive statements (DROP, TRUNCATE, DELETE or UPDATE without WHERE, ALTER … DROP, FLUSHALL), so that one typo cannot wipe data.
36. As a user on production, I want to confirm every statement that modifies data, so that every write is deliberate.
37. As a user, I want a write hidden later in a batch to be caught, so that a harmless first line cannot hide a `DELETE`.
38. As an administrator, I want Safety flags that only tighten profiles, so that a global or per-Connection setting can add protection that nothing removes.

## Implementation Decisions

- Engine Sessions are keyed by Connection and slot (explorer, Editor tab, Agent). The explorer Session also serves the explorer, Catalog loads and Jobs. A tab Session opens on its first run; per-tab sessions follow the global or per-Connection setting.
- Each Session has an async side with a ticket queue (executes start in issue order) and a stream lock holding at most one open stream; a new execute closes the previous stream and marks its Result cut short. Other-database connections are opened on demand per Session.
- Results are shared, lock-protected objects with status, rows, affected count, elapsed time, Page size, cut-short flag, messages and a memory reservation against a per-Result cap and a global atomic budget.
- Fetching reads up to 1,024 rows per chunk until a page is full; fetch all loops pages up to the row cap and probes one row at the cap.
- Scripts start their first statement immediately (taking the next ticket) and run the rest as one Operation, waiting for each statement's first page or outcome.
- Transactions are a state machine in the engine fed by the Dialect's transaction rules (DDL commits, errors abort, nested begin, savepoint replacement, savepoint starts a transaction) and by classified statements; the engine runs the Dialect's begin, savepoint and read-only statements itself. Transaction commands run in the Session's order.
- The UI resolves the effective profile (the Connection's profile tightened by Safety flags), runs confirmations before starting a run, draws the transaction controls and the status line, and owns the end-of-transaction prompts with the 5 s forced disconnect and the 15 s idle check.
- Profiles are a core type with three built-ins; the store keeps user profiles and overrides in `profiles.toml`; the engine loads and saves them on its `sqlr.db` worker.
- Server-side sort and filter wrap the original statement through the Dialect's sort and filter wrappers.
- Cancellation goes through each Driver's cancel handle; Operations and Jobs are tracked in the Operation registry.

## Testing Decisions

- Engine seam: the engine's public interface with the fake relational Driver scripted per test: commit modes, savepoints, read-only refusal and server switch, per-tab and shared Sessions, closing and disconnecting, lost connections, PostgreSQL-style aborts, MySQL DDL commits, other-database refusal, page reads outside Manual transactions, scripts with stop and continue, cancellation, paging and memory budgets. Prior art: the engine's transaction, script, operations and integration suites.
- Real servers: engine transaction tests against SQL Server, and Driver suites against the shared PostgreSQL, MySQL and MariaDB services for cancel, streaming and paging.
- Core seam: unit tests of the statement classifier (batches, CTEs, EXPLAIN, FOR UPDATE, comments and strings) and of the built-in profiles.
- App seam: `egui_kittest` and app-level tests for confirmations (destructive first, then writes, batches, long lists), the transaction prompts (tab close, disconnect, quit, auto-commit switch, idle timeout, forced disconnect) and closing tabs closing their Sessions.

## Acceptance Criteria

- [x] With per-editor sessions on, each Editor tab runs on its own Session and tabs run concurrently.
- [x] With the setting off, tabs share the explorer Session; tabs that already have a Session keep it until closed.
- [x] Executes on one Session start in issue order; a new statement cuts the previous Result short and says so.
- [x] The default Page size is 500, with presets 100, 500, 1000 and 5000 and a custom value.
- [x] Scrolling near the end fetches the next page; Fetch all is cancellable and stops at the row cap with a note.
- [ ] The default per-Result memory budget is 256 MB.
- [x] A per-Result and a global memory budget are configurable, and 0 disables each.
- [ ] Chunks beyond a memory budget spill to a temporary file read back through mmap instead of being discarded.
- [x] Closing a Result frees its buffers at once.
- [x] Complete Results sort locally; partial Results offer Sort on server through the Dialect's wrapper; the WHERE filter reruns on the server.
- [x] Stop requests a server-side cancel on every Driver that declares the capability.
- [x] Scripts run statement by statement, write to Output, and stop at the first error by default or continue by setting.
- [x] Auto, Manual and Smart commit modes behave as specified, including typed BEGIN in Auto.
- [x] Savepoints can be created and rolled back to while a transaction is open in Manual and Smart mode.
- [x] The status bar shows "Transaction: N statements · since HH:MM" and the explorer marks the Connection.
- [x] A failed statement on PostgreSQL leaves the transaction Failed with only Rollback possible.
- [x] Closing a tab, disconnecting, reconnecting and quitting with an open transaction ask Commit, Roll back or Cancel.
- [x] A disconnect or reconnect still waiting 5 s after the answer is forced through.
- [x] The idle transaction timeout prompts after the profile's minutes without activity.
- [x] Read-only is applied on the server where the Dialect supports it and enforced by the client classifier.
- [x] Built-in profiles Development (Auto), Test (Smart, amber) and Production (Manual, red, writes and destructive confirmed) exist, are editable and resettable, and user profiles live in `profiles.toml`.
- [x] Destructive statements and, when the profile asks, all writes are confirmed before running, batches included.
- [x] Safety flags only tighten the effective profile.
- [x] Drivers without transactions run in Auto whatever the profile says.

## Out of Scope

- Scheduled execution of statements or scripts.
- Spilling large Results to disk.
- Server sessions and locks administration (the administration panels).

## Further Notes

This task records retroactively the work that built the area as it exists today. The unticked criteria come from the earlier design document: the code uses a 128 MiB per-Result default (512 MiB global) and truncates at the budget instead of spilling to disk.
