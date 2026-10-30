---
status: in-progress
---

# Verify and complete the workspace bug fixes

## Specs

- [Architecture](../specs/architecture.md)
- [Connections](../specs/connections.md)
- [Query execution](../specs/query-execution.md)
- [Query editor](../specs/query-editor.md)
- [Results and editing](../specs/results-and-editing.md)
- [Drivers](../specs/drivers.md)
- [Agents and terminal](../specs/agents-and-terminal.md)
- [Data and schema tools](../specs/data-and-schema-tools.md)
- [Settings and workspace](../specs/settings-and-workspace.md)
- [Explorer and objects](../specs/explorer-and-objects.md)
- [Diagnostics](../specs/diagnostics.md)

## Problem Statement

A broad bug hunt produced local changes across the whole workspace. Passing existing
tests and repeated clean review cycles did not establish that each claimed fix solved
a real problem, preserved the intended behaviour, or reached the production path.
The user requested one aggregate review of all unpushed changes, corrections for every
confirmed finding, and inclusion of later commits added to main during the review.

The audit covers 41 commits between `4031ca3` and main at `e69aeaf`, followed by the
review corrections. The original audit record was `find-bugs.md`; this completed task
replaces that working diary and its repetitive clean-cycle entries.

## Solution

Validate the combined behaviour against the specs and real failure cases. Retain
useful bug fixes, distinguish minor UX changes from functional repairs, and correct
incomplete fixes with regression tests that fail first. Update the living specs and
record the evidence and practical limits in this task.

## User Stories

1. As a user importing data, I want BOMs, blank CSV fields, large numbers and dates preserved, so that a transfer keeps the original values.
2. As a user running SQL, I want classification and formatting to preserve statement meaning, so that read-only checks and confirmations match what will execute.
3. As a user saving Connections, I want failed credential deletion to remain retryable, so that secrets stay associated with an identifiable Connection.
4. As a user working in a Session, I want its transaction state to follow the server, so that commit, rollback and savepoint controls remain trustworthy.
5. As a user exporting or importing data, I want exact values, bounded rejection handling and cancellation, so that jobs report partial work accurately.
6. As a Sidecar driver user, I want cancellation, disconnect and shutdown to release the right resources, so that unrelated work continues and cursors are closed.
7. As a user installing Drivers, I want versions and redirects validated, so that updating or removing one version preserves the remaining install.
8. As a user exposing Connections to Agents, I want TLS, profile and Result ownership policies enforced on the actual tool paths, so that Agents respect the configured safeguards.
9. As an Integrated terminal user, I want input, selection, cell backgrounds and scrollback to remain correct, so that terminal applications behave consistently.
10. As a PostgreSQL user, I want parameterized queries and modifying CTEs to work inside my transaction, so that query handling preserves earlier uncommitted work.
11. As a MySQL or MariaDB user, I want syntax and generated DDL to follow the server's modes, so that generated columns and spatial indexes round-trip correctly.
12. As a SQLite user, I want virtual-table storage protected and invalid AUTOINCREMENT definitions diagnosed, so that object editing keeps the database usable.
13. As a Redis user, I want command options and reply types interpreted correctly, so that confirmations reflect actual effects and the console remains responsive.
14. As an Oracle or SQL Server user, I want NULL LOBs, time zones and statement terminators preserved, so that results and generated writes remain valid.
15. As a user of Elasticsearch, Snowflake, ClickHouse or JDBC, I want partial failures and metadata reported faithfully, so that a successful-looking result does not hide a failure.
16. As a MongoDB, DuckDB or HANA user, I want supported values and column metadata decoded accurately, so that results remain readable and editing rules are correct.
17. As a user reopening Connections or objects, I want the right tab opened and focused, so that the requested surface comes forward.
18. As a user editing results or SQL, I want selections, hex input, document pages and CRLF wrapping handled safely, so that ordinary input does not panic or hide data.
19. As a user comparing schemas, I want nested partitions and their definitions preserved, so that the migration runs once and a repeated compare converges.

## Implementation Decisions

- Review the aggregate diff against the tracked upstream, then incorporate and review the later main commits before integration. Keep existing unrelated work intact.
- Preserve the Driver interfaces and workspace dependencies. Use the existing transaction savepoints, partition-attachment operations and typed errors instead of introducing a new partition model or production-only test hooks.
- Make Sidecar stream registration and disconnect agree atomically on connection ownership, releasing locks before asynchronous cleanup.
- Compare transport protection using actual Driver mode spellings and effective defaults. Share strict color validation between the production configuration paths.
- Preserve unsupported numeric ranges through explicit text mappings and warnings rather than invalid destination DDL or silent rounding.
- Keep every partition as its own table definition. Attach tables after creation and table changes; emit inherited triggers and foreign keys once.
- Treat the first-free-savepoint-name adjustment as a small UX improvement, not a repaired execution failure.
- Keep lasting behaviour in the linked specs. This task records scope, regression evidence and validation instead of repeated claims that another review cycle was clean.

## Testing Decisions

- **Core public APIs:** exercise delimited input, statement classification and transfer mappings with concrete values and independently specified expected outputs.
- **Engine public interface:** use the existing Fake Drivers and controlled fetch notifications to reproduce queued cancellation and closing without timing guesses.
- **Driver and protocol interfaces:** run PostgreSQL execution and snapshot migrations on a real PostgreSQL 17 server; drive Sidecar framing with a real SQLite stream paused by a test-only wrapper; test MCP through its HTTP tool interface.
- **Existing workspace regression suite:** retain the widget, app, terminal, store, Driver and engine tests, including the later main fixes. Run formatting, workspace Clippy and dependency checks on the integrated tree.
- Use a separate build target for final verification, so concurrent work on main cannot replace the libraries or test executables under review.
- Observe new regressions fail before applying their corrections. Existing passing tests alone are insufficient evidence for a bugfix.

## Acceptance Criteria

- [x] Review the original and later main commits as one combined change, covering all 19 audit areas.
- [x] Identify concrete failures and distinguish cosmetic changes from functional fixes.
- [x] Correct the confirmed transaction, TLS, classification, transfer, stream ownership, query supersession, partition and MCP validation defects.
- [x] Keep main's equivalent CSV fix and regression without duplicating them.
- [x] Add or strengthen regression tests at the existing public seams and observe the faulty cases fail first.
- [x] Update the affected living specs alongside the corrections.
- [x] Formatting, workspace Clippy, single egui version and no-OpenSSL checks pass.
- [ ] The final workspace suite passes with external-service tests disabled.
- [ ] PostgreSQL execution and snapshot-migration tests pass on the integrated tree.
- [ ] This task is marked done and committed before merging the worktree into main.

## Out of Scope

New features, Driver protocol redesigns and a new partition data model are unnecessary
for these fixes. A fresh run of the entire multi-database container matrix is outside
the focused verification recorded here; earlier audit runs are historical evidence,
not a claim that the final review tree ran that matrix.

## Further Notes

The work is isolated in the review worktree, realigned with main, then integrated into
local main after verification and this task's closure. The temporary worktree and its
branch are removed afterwards. No push is part of the task.

## Aggregate Review

Most changes address real defects with small, relevant fixes. The initial fast
workspace suite passed all 2,487 tests, but additional regression cases exposed
incomplete fixes and new regressions. The findings below have been corrected or
incorporated from main, with failing cases observed before applying the corrections.

### Standards

**One finding, resolved:** behaviour and architectural decisions had changed without
updating the living specs, contrary to `CLAUDE.md`'s workflow. Updated the specs for
connections, query execution, drivers, agents, and data/schema tools. These now
describe the transfer limits, TLS policy, stream ownership and partition migration
behaviour. No new dependencies or production test hooks were needed.

The savepoint-name change is a small UX improvement: the earlier algorithm already
chose a free name. Choosing `sp1` rather than `sp3` when only `sp2` exists does not fix
an execution failure. Its description as a bugfix overstated its importance; the
behaviour is harmless and remains consistent with the toolbar spec.

### Task

| Priority | Finding | Correction and evidence |
| --- | --- | --- |
| P1 | PostgreSQL's new parameterized `WITH` wrapper probe aborted an explicit transaction before trying the binary fallback. | Protect the probe with a savepoint. A real PostgreSQL test executes a modifying CTE after earlier uncommitted work, reads all expected rows and verifies that rollback preserves only the original committed row. |
| P1 | MCP TLS ranking used PostgreSQL spellings for MySQL modes. `verify_identity` to `disabled` was accepted. An omitted field also bypassed the comparison with the manifest default. | Recognize the actual mode spellings and compare an existing Connection against its effective default. Unit and HTTP protocol regressions cover both paths. |
| P2 | `EXPLAIN WITH … DELETE/INSERT` was treated as destructive without `ANALYZE`. The later PRAGMA fix still rejected read-only PRAGMAs following a semicolon. | Find the explained main statement after its CTEs and reset the first-word exemption at each statement boundary. Tests retain detection of genuinely hidden writes. |
| P2 | New transfer mappings could emit invalid destination DDL or silently constrain an unconstrained decimal. | Respect sized-string and decimal limits; use the destination's text type with a warning when needed. Tests cover SQL Server `varchar(5000)`, Oracle/HANA string limits, oversized precision and unconstrained decimals. |
| P2 | Checking whether a sidecar connection existed before registering its stream left a race with disconnect. Disconnect itself removed the connection after awaiting stream cleanup. | Remove connection ownership first and register streams atomically against that ownership. A protocol test pauses a real SQLite stream during registration, disconnects, and verifies cancellation plus one explicit stream close. Shutdown follows the same ownership order. |
| P2 | Headerless CSV with leading blank lines followed by a wide record produced extra one-cell rows. | Main already contained the same correction when the worktree was realigned. Kept main's fix and regression instead of duplicating them. Single-column blank rows remain supported. |
| P2 | The new query-supersession fix tracked only the latest issued query. Cancelling or closing a third query could let fetch-all block the second queued query indefinitely. | Recover the next viable queued query on the same Session. A deterministic engine regression covers both cancellation and closing while a prior fetch is blocked. |
| P2 | Partition migration could duplicate inherited indexes, skip nested children and lose child definitions. Appending child indexes to the parent's metadata did not preserve the actual hierarchy. | Create each table from its complete definition, then attach partitions. Avoid replaying inherited triggers and foreign keys. A real PostgreSQL round trip covers nested partitions, parent/child indexes, a child check, comments, triggers and a foreign key, and converges to an empty diff. The earlier child-constraint limitation is resolved. |
| P2 | The new color-validation fix changed a parser unused by the MCP configuration tools. Production writes still accepted `[1,2,3,256]`. | Share strict RGB validation across settings, Connections and profiles. All three public protocol tests failed before the correction and pass afterwards. |

Standards: one resolved finding. Task: nine resolved finding groups, including the
CSV correction already supplied by main. The highest Task risks were transaction
loss and the ineffective TLS downgrade restriction.

### Validation

- Regression cases were run against the faulty code before each correction.
- The MCP HTTP protocol suite passes all 30 tests.
- The final workspace, PostgreSQL and quality-gate results are recorded below after
  verification of the integrated tree. An earlier shared-target run was discarded
  after concurrent compilation on main replaced a dependency with older code; final
  verification uses an isolated build target.

The fast workspace run disables external-service tests. PostgreSQL execution and
snapshot migrations additionally run against an isolated PostgreSQL 17 server.
The review does not substitute that focused verification for a fresh run of the
entire multi-database container matrix. DuckDB `BIGNUM` still displays opaque binary
storage, as documented; casting it to `VARCHAR` in a query provides decimal text.

Reference limits were checked against the official [SQL Server character-type
documentation](https://learn.microsoft.com/en-us/sql/t-sql/data-types/nchar-and-nvarchar-transact-sql),
[SQL Server decimals](https://learn.microsoft.com/en-us/sql/t-sql/data-types/decimal-and-numeric-transact-sql),
[Oracle types](https://docs.oracle.com/en/database/oracle/oracle-database/26/sqlqr/Data-Types.html),
[HANA numeric types](https://help.sap.com/docs/SAP_HANA_PLATFORM/4fe29514fd584807ac9f2a04f6754767/4ee2f261e9c44003807d08ccc2e249ac.html),
[Snowflake numbers](https://docs.snowflake.com/en/sql-reference/data-types-numeric) and
[ClickHouse decimals](https://clickhouse.com/docs/reference/data-types/decimal).
