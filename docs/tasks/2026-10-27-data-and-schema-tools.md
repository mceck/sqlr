---
status: done
---

# Data and schema tools

## Specs

- [Data and schema tools](../specs/data-and-schema-tools.md)
- [Explorer and objects](../specs/explorer-and-objects.md)
- [Query execution](../specs/query-execution.md)
- [Drivers](../specs/drivers.md)
- [Settings and workspace](../specs/settings-and-workspace.md)

## Problem Statement

A database client is used for more than running statements. People back up a database before a risky change, copy a table from staging to a local server, load a spreadsheet a colleague sent, seed a test database with realistic rows, check what changed between two environments before a release, look at the shape of a schema in a diagram, and find out why one query is slow. Doing any of these by hand means remembering several command-line programs, their flags and their password handling; copying data by hand means writing throwaway scripts; understanding a plan means reading a nested JSON document. Each of these is where mistakes are expensive: a dump that looks complete but is truncated, a restore that wipes the wrong database, a transfer that silently drops a column, a migration script nobody reviewed.

## Solution

Dump and Restore wrap the database's own command-line tools: sqlr finds them, builds the command, passes the password outside the command line, streams the log, shows progress, and can stop the whole process group. Output goes to a partial file that is renamed only on success.

Data transfer moves rows between any two Endpoints through sqlr's own drivers: tables, queries, CSV, TSV, JSON, JSON Lines, workbooks, Parquet and SQL INSERT files, document collections and key-value keys. It maps columns, converts values to target types, generates and maps the DDL of a new table with warnings where a type loses something, writes in batches with commit, truncate, upsert, constraint disabling and rejected rows, keeps memory bounded and stops cleanly.

Import and Export are presets of the same engine. A test-data generator fills a table with deterministic rows, foreign keys included. Schema compare reads two schemas, shows a tree of differences and a side-by-side definition diff, and generates a migration script that opens in the editor. ER diagrams are laid out from foreign keys, can be rearranged and saved, and export to SVG. Explain and Explain Analyze show the plan in the tab's Plan pane with warnings, a hot path and operator details, running on the tab's own Session and rolling back analyzed writes.

## User Stories

1. As a user about to change a production schema, I want to dump the database in a native format, so that I have a backup the server's own tools can restore.
2. As a PostgreSQL user, I want plain, custom, directory and tar dumps, so that I can pick the format my restore procedure expects.
3. As a MySQL user, I want a SQL dump with routines, events and triggers, so that a restore brings back more than tables.
4. As a SQLite user, I want a SQL dump or a consistent binary copy, so that I can choose portability or an exact copy.
5. As a user, I want to choose schema only, data only, clean, create, compression, encoding, parallel jobs, owner and privileges, so that the dump fits its purpose.
6. As a user, I want a clean dump confirmed before it runs, so that I do not generate a script that drops everything by accident.
7. As a user, I want the dump's output to stream into a log, so that I can see what it is doing and what went wrong.
8. As a user, I want progress for a binary SQLite backup and log-based progress otherwise, so that I know a long dump is moving.
9. As a user, I want Cancel to stop the tool and all of its child processes, so that a stopped dump leaves nothing running.
10. As a user, I want a cancelled or failed dump to leave no file at the destination, so that a partial file is never mistaken for a backup.
11. As a user whose tool is not installed, I want a message that names the program and how to install it, so that I can fix it without searching.
12. As a Homebrew or Postgres.app user, I want sqlr to find the tool without changing my PATH, so that a GUI launch works.
13. As a user with a nonstandard install, I want to set the executable path for a Connection, so that my tool is used.
14. As a security-conscious user, I want the password passed through the environment or a temporary defaults file, and never printed, so that it cannot leak through the command line or the log.
15. As a user restoring a dump, I want the right program chosen from the file, so that a plain script and an archive both work.
16. As a user restoring into a database, I want clean, create, schema only, data only, single transaction, parallel jobs and exit on error, so that the restore matches the case.
17. As a SQLite user, I want a restore that replaces the database file to warn me first, so that I cannot lose the current data by mistake.
18. As a user, I want to copy rows from a table to another table, so that I can move data between servers without writing a script.
19. As a user, I want to run a query as a transfer source, so that I can copy a filtered or joined set.
20. As a user, I want to load a CSV, TSV, JSON, JSON Lines, workbook or Parquet file, so that data from outside the database comes in.
21. As a user, I want to export to CSV, TSV, JSON, JSON Lines, SQL INSERT or Markdown, so that the file fits the next tool.
22. As a user, I want a sample preview and a suggested file name, so that I see the output before writing it.
23. As a user with a large result, I want to export the loaded rows or rerun the whole statement, so that I control whether the export is complete.
24. As a user, I want to create a new target table with generated DDL that I can edit, so that a transfer to a new server is one step.
25. As a user moving between databases, I want source types mapped to the target's types, so that the new table is usable.
26. As a user, I want a warning when a type loses something in the mapping, so that I know a time zone was dropped or a decimal stays text.
27. As a user, I want to map each source column to a target column or skip it, so that mismatched names and extra columns are handled.
28. As a user, I want to trim, change case, turn blanks into NULL and set a constant, so that common cleanups need no separate step.
29. As a user, I want values converted to the target column type, so that a CSV's text becomes the right integer, boolean, timestamp or binary value.
30. As a user, I want a batch size and the choice to commit per batch or once, so that a failure costs the right amount of work.
31. As a user, I want truncate, upsert by key and disabled constraints, with the risky ones confirmed, so that reloading a table is a single operation.
32. As a user, I want a bad row to be skipped and saved with its reason, so that one malformed line does not stop a large import.
33. As a user, I want a summary of rows read, written and rejected, so that I know exactly what happened.
34. As a user, I want to stop a transfer and keep the committed batches, so that I can resume from a known point.
35. As a user, I want the transfer to use bounded memory whatever the size, so that a large table does not exhaust the app.
36. As a user, I want a five-step wizard that keeps my choices when I go back, so that a complex transfer is reviewable before it runs.
37. As a user, I want the wizard to refuse copying a table onto itself on the same Connection and to tell me to use `INSERT … SELECT`, so that the mistake is explained.
38. As a developer, I want to fill a test table with generated rows at a chosen count and seed, so that tests and demos have realistic data.
39. As a developer, I want generators chosen from column names and types, so that a table is seeded without configuring every column.
40. As a developer, I want unique values and a NULL ratio per column, so that constraints are satisfied and missing data is simulated.
41. As a developer, I want foreign keys filled from the referenced table, so that the generated rows pass the constraints.
42. As a developer, I want a preview of generated rows and a repeatable seed, so that I can check and reproduce the data.
43. As a developer, I want the generator to stop with a clear error when a unique domain is too small, so that I know why it failed.
44. As a user, I want to compare two schemas of the same database kind, so that I can see what differs before a release.
45. As a user, I want added, removed and changed objects in a tree, with a side-by-side definition diff, so that I can review each difference.
46. As a user, I want a migration script generated from the differences, opened in the editor and never run automatically, so that I review it first.
47. As a user, I want destructive steps called out in the script, so that I notice what loses data.
48. As a user, I want to exclude objects from the script, so that the migration covers only what I intend.
49. As a user, I want the comparison to write nothing to either database, so that looking is always safe.
50. As a user, I want an ER diagram laid out from foreign keys, so that the schema's shape is readable without manual drawing.
51. As a user, I want to drag tables and have the layout saved per schema, so that my diagram stays recognizable.
52. As a user, I want to export the diagram as SVG, so that I can put it in documentation.
53. As a user, I want Explain and Explain Analyze from the tab, so that I see the plan for the statement I am working on.
54. As a user, I want the plan summarized with time, cost, rows and warnings, so that I know where to look without reading JSON.
55. As a user, I want a hot path bar and operator cards, so that the expensive part of the plan is obvious.
56. As a user, I want the raw plan one toggle away, so that I can copy it into another tool.
57. As a user, I want Analyze on a writing statement to ask first and roll back, so that I can measure it without changing data.
58. As a user, I want a running Explain to be stoppable, so that a heavy Analyze does not hold the tab.
59. As a user, I want the plan to stay as a snapshot and be marked changed when I edit the statement, so that I do not lose it.
60. As a user, I want Explain to run on the tab's own Session, so that the plan reflects its settings and transaction state.

## Implementation Decisions

- The core holds the tool command construction as data: the program, arguments, environment and temporary files for each format and option set, with a display form that omits the password. The engine resolves Credentials and SSH tunnels, discovers the program, runs it in its own process group, streams output into the Job log, reports progress and turns Cancel into a process-group signal.
- A dump writes to a hidden partial path and renames it over the destination only on success; failure and cancel delete the partial path, including a directory written by a directory-format dump.
- Tool discovery searches `PATH`, then the usual install locations, preferring the newest Postgres.app; a per-Connection executable overrides discovery. A missing tool produces an install hint per family.
- The Dump, Restore, transfer, generator, compare, ER and Explain specifications are core types so that dialogs, Saved tasks and the Agent interface share one representation.
- The transfer engine is a pipeline over the Driver interface: a source adapter yields chunks; mapping and conversion run in the core; a writer batches rows per target, flushes at a byte threshold, creates a new table first, optionally truncates, upserts through the Dialect and uses a savepoint per row when bad rows are skipped. Table targets require a transactional, writable Session with no open transaction.
- Import and Export dialogs build the same transfer specification and call the same engine entry points; they differ only in which Endpoint kinds they offer and how many options they show.
- The generator is a core component producing deterministic rows from a seed, with unique-by-construction generators and a bounded retry for uniqueness. The engine supplies foreign-key values by reading the referenced table once, and writes generated rows through the transfer engine.
- Schema snapshots, the Diff, the ER model and the plan tree are pure core structures. The engine reads snapshots through Drivers and parses raw plans with the parser the Driver names; the migration script is generated with the target Driver's DDL.
- The ER layout is a layered graph layout with barycenter ordering, deterministic tie-breaking and cycle breaking. Layouts are stored per Connection, database, schema and table set through the store worker.
- Explain runs on the Editor tab's Session and takes its turn there. The Driver builds the engine-specific Explain command and wraps an analyzed write in a transaction it always rolls back; the engine refuses Analyze on a non-read where the engine does, and refuses it while a transaction is open.
- The Plan pane is a state machine per tab: the plan, whether it was analyzed, the statement it was computed from, a running Job and an error. Editing the statement compares against the stored statement to mark the plan changed; Recompute and Retry start a new Job.
- Dialogs are tabs or modal panels over the Job they started, polling progress and following the Job until it settles. Native file and folder pickers are opened asynchronously and polled so that the UI thread never waits.
- Saved tasks, their runs and ER layouts are stored through the store worker; task runs are trimmed to the newest 50 per task, and a run log is cut from the end at 1 MiB on a character boundary.

## Testing Decisions

- Core unit seam: type mapping across the three relational dialects including warnings; auto-mapping by name; transforms and conversions per target class; generator determinism, uniqueness, NULL ratio and foreign-key picks; command construction and argument escaping for every format and option combination; snapshot normalization, diff classification, exclusions and migration script ordering; ER layout determinism and SVG output; plan parsing for each engine, warning thresholds and hot path selection. Prior art: the existing per-module test suites in the core.
- Engine seam: the engine's public interface with the fake Driver for pipeline behaviour, and the shared PostgreSQL, MySQL, MariaDB and SQLite services for real transfers, new-table creation, upsert, truncate, rejected rows, constraint disabling and cancellation. Prior art: the transfer, tools, compare, explain and tasks engine suites.
- Native tools seam: command construction is tested without running anything; a small fake program stands in for the tool where process handling, log streaming and process-group cancellation are tested. Prior art: the engine's tool registry tests.
- App seam: `egui_kittest` and app-level tests for the Dump and Restore dialogs, the Export and Import dialogs, every wizard step and its validation, the generator dialog, the compare view, the ER canvas and the Plan pane's summary, warnings, hot path and stale marker. Prior art: the app's dialog and pane tests.
- Storage seam: Saved task, Task run and ER layout round trips through the store worker, including retention and log truncation. Prior art: the store's tasks and layouts tests.

## Acceptance Criteria

- [x] Dump offers plain, custom, directory and tar for PostgreSQL, a SQL script for MySQL, and a SQL script or binary copy for SQLite.
- [x] Dump options cover schema only or data only, clean, if exists, create, compression, encoding, parallel jobs, no owner, no privileges, extra arguments and an executable override, with the per-engine defaults.
- [x] A clean dump asks for confirmation before running.
- [x] A dump writes to a hidden partial path and renames it only when the tool succeeds; a failed or cancelled dump leaves no file at the destination.
- [x] A dump's command line is written to the Job log with the password omitted.
- [x] PostgreSQL passwords go through the environment and MySQL passwords through a temporary defaults file with owner-only permissions.
- [x] The tool is found on `PATH` and in the usual macOS, Linux and Windows install locations, with a per-Connection executable override and an install hint when missing.
- [x] Cancel terminates the tool's process group.
- [x] Restore chooses the plain-script or archive path from the input file and offers clean, if exists, create, schema only, data only, single transaction, parallel jobs, no owner, no privileges and exit on error.
- [x] A restore that replaces a SQLite database file warns before running.
- [x] A transfer runs between tables, queries, CSV, TSV, JSON, JSON Lines, workbook and Parquet files, document collections, key-value keys and the generator.
- [x] A transfer target can be an existing table, a new table with generated and editable DDL, a file, a workbook sheet, a Parquet file or a collection.
- [x] A table or query source on the same Connection as a table target is refused with advice to use `INSERT … SELECT`; a query source must be a read.
- [x] Type mapping between dialects produces the documented target types and a warning for every lossy or unknown mapping.
- [x] Column mapping maps by name ignoring case, skips unmatched columns, and offers trim, upper, lower, blank-to-NULL and constant transforms.
- [x] Conversions handle integers, decimals, booleans, binary, timestamps folded to UTC and text, with NULL preserved at every type.
- [x] Batch size, commit per batch, truncate, upsert, disable constraints and skip bad rows with a rejected rows file all work; constraints are restored after a cancel.
- [x] A rejected rows CSV holds the line, the reason and the source values, and the transfer stops at 1,000 rejections.
- [x] A transfer ends with a summary of rows read, written and rejected, and a cancel keeps the batches already committed.
- [x] A transfer holds bounded memory through 500-row chunks, 1,000-row reads from a loaded Result and 256 KiB write flushes.
- [x] The wizard runs Source, Target, Mapping, Options and Review, keeps choices on Back and confirms truncate and constraint disabling.
- [x] Export writes CSV, TSV, JSON array, JSON Lines, SQL INSERT or Markdown with a sample preview and a suggested file name.
- [x] A partial Result exports the loaded rows or reruns the whole read statement.
- [x] Import writes CSV, TSV, JSON array or JSON Lines with mapping, batch, commit, skip bad rows, truncate and a rejected file.
- [x] The generator picks a generator per column from name and type, sets rows and seed, NULL ratio and uniqueness, and previews 20 rows.
- [x] Foreign-key generators read up to 100,000 values from the referenced table, and generated rows are written through the transfer engine.
- [x] The same generator specification and seed produce the same rows, and a unique domain that is too small stops with a clear error.
- [x] Compare takes two Connections of the same Driver and shows added, removed and changed counts and a tree of differences with exclusion checkboxes.
- [x] The side-by-side definition diff shows the target on the left and the source on the right, and definitions are normalized before comparison.
- [x] The migration script is deterministic, ordered as specified, lists destructive steps, skips excluded objects, uses the target DDL and opens in an unsaved editor tab.
- [x] Compare and the migration script write nothing to either database.
- [x] An ER diagram marks primary and foreign keys, draws a crow's foot or single stroke per uniqueness, and lays tables out in layers from foreign keys.
- [x] Dragging a table is saved per Connection, database, schema and table set, and Relayout discards the saved positions.
- [x] An identical table set reopens the same ER tab, and Export writes an SVG file.
- [x] Explain and Explain Analyze run on the tab's own Session from the split button, the Query menu and their shortcuts, for a Driver that supports Explain.
- [x] Only one read, writing, `TABLE` or `VALUES` statement is explained; longer scripts and parameterized statements are refused with a reason.
- [x] Analyze of a writing statement asks inline and runs inside a transaction that is always rolled back; Analyze is refused while a transaction is open.
- [x] A running Explain can be stopped, and a new plan dims the previous one.
- [x] The Plan pane shows the statement, the Estimated or Analyzed chip, the changed chip, Copy, Raw, Collapse and Expand, Recompute, timings, cost, rows, warnings, operator mix and an Analyze action for an estimated plan.
- [x] Warnings cover a 10x row estimate error, a scan over 10,000 rows, a node run 1,000 times, and spills including MySQL temporary tables and filesort.
- [x] The hot path bar, HOT badges, operator cards, inspector and raw view are present.
- [ ] SAP HANA's own `BACKUP DATA` and `RECOVER` commands are offered as Dump and Restore.

## Out of Scope

- Scheduled or recurring task execution.
- Applying a migration script automatically, or recording which migrations ran.
- A visual query builder over the ER diagram.
- Editing the plan, or forcing an index hint from the Plan pane.
- Comparing across database engines.

## Further Notes

This task records retroactively the area as it exists. SAP HANA native Dump and Restore integration remains to do. Dump commands are logged when the job starts. Saved tasks exist in the engine and storage, with MCP read and delete access; script runs are rejected. ER diagrams export as SVG.
