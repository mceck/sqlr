---
status: done
---

# Application architecture, threading and performance budgets

## Specs

- [Architecture](../specs/architecture.md)
- [Query execution](../specs/query-execution.md)
- [Drivers](../specs/drivers.md)
- [Diagnostics](../specs/diagnostics.md)

## Problem Statement

Database clients people use every day are heavy: they start slowly, take hundreds of megabytes before the first query, stutter when a large result scrolls, and freeze when a server is slow or a network drops. A failing driver can take the whole application down with it. Users who switch between PostgreSQL, MySQL, MongoDB and Redis during a day end up with several such tools open at once.

## Solution

sqlr is one native application for relational, document and key-value databases, built so that lightness is measurable and kept: it starts in under a second, idles under 150 MB, scrolls a million rows at 60 fps and ships as one binary under 75 MB with its drivers included. The window always stays responsive because all database, keychain and file work happens off the UI thread, every piece of in-flight work can be seen and cancelled, and a failing driver fails only its own operation.

## User Stories

1. As a developer, I want sqlr to open in under a second, so that checking a value does not cost me a coffee break.
2. As a developer with many tools open, I want sqlr to use under 150 MB at idle with two connections, so that it fits next to my IDE and browser.
3. As an analyst, I want to scroll a result of a million rows smoothly, so that I can skim large tables without exporting them.
4. As a user, I want autocomplete and opening a table to feel instant, so that the client never slows my typing.
5. As a user, I want one download with no Java, Electron or other runtime to install, so that installing sqlr is trivial.
6. As a user, I want the window to stay responsive while a query runs, so that I can keep working in other tabs.
7. As a user, I want the window to stay responsive while the OS asks for keychain permission, so that the app never looks frozen.
8. As a user on a flaky network, I want a stalled connection attempt to end when I cancel it, so that I do not wait for a timeout.
9. As a user, I want to see every running connect, query, fetch, script and background job in one panel, so that I know what the app is doing.
10. As a user, I want to cancel any single running operation, so that one slow thing does not force me to cancel everything.
11. As a user, I want "Cancel all" to ask once before stopping everything, so that I do not stop work by accident.
12. As a user, I want finished operations to stay listed for a while, so that I can see what just ended.
13. As a user, I want Stop to cancel a query on the server where the database allows it, so that the server stops working too.
14. As a user, I want a driver bug to fail only the operation that hit it, so that my other tabs and connections keep working.
15. As a user of a third-party driver, I want a crashing driver process to fail only its connections and offer a restart, so that the app survives.
16. As a user, I want errors shown where they matter (a syntax error underlined in the editor, a lost connection with reconnect, a permission error on the explorer node), so that I can act on them.
17. As a user, I want a lost connection to say that its open transaction is gone, so that I do not believe my changes are pending.
18. As a user, I want sqlr to keep working with an in-memory history when its local database cannot be opened, so that a broken file does not stop me.
19. As a user, I want large text values and decimals to keep full precision, so that what I see is what the server holds.
20. As a driver author, I want the same data format in process and over the sidecar protocol, so that my driver works both ways without transcoding.
21. As a driver author, I want a conformance suite I can run against a real database, so that I know my driver behaves like the bundled ones.
22. As a contributor, I want clear layering rules (core with no I/O, widgets that only return actions, the app as the only wiring point), so that I know where new code belongs.
23. As a contributor, I want the build to fail if OpenSSL or a second egui version sneaks in, so that the dependency rules hold without reviews catching them.
24. As a contributor, I want measured performance budgets with a harness that fails on regression, so that lightness stays a fact and not a slogan.
25. As a contributor, I want shared database services for tests, so that the full suite runs fast without a server per test.
26. As a contributor without Docker, I want a switch that skips service-backed tests, so that I can still run everything else.
27. As a privacy-conscious user, I want no telemetry and no network requests from diagnostics, so that my usage stays on my machine.

## Implementation Decisions

- Components by role: core (domain types, driver and Dialect interfaces, statement classification, chunk formats; no I/O, no UI), Drivers (depend on core only), the sidecar protocol and client, the driver registry, the driver SDK, the store (files, vault, `sqlr.db`), the engine (runtime, Sessions, Results, Catalog cache, Operations, Jobs), UI widgets (take data, return actions), the MCP server, the Integrated terminal, and the app as the only wiring point.
- The engine is a facade whose methods return immediately. It sends commands over an unbounded channel to a 2-worker async runtime on its own thread; each command runs in its own task. Shared state (Results, Catalog cache, Operations, transaction state, Job state) is read by the UI under short locks every frame. Work on one Session is ordered by tickets.
- `sqlr.db` writes go through a sequential worker; connection files and the vault through a store worker thread in the app whose replies the UI polls each frame.
- The engine requests a repaint on every shared-state change; egui coalesces requests into the next frame.
- An Operation registry records every in-flight Operation with kind, label, Connection, progress and state, and maps cancellation to the right target (connect signal, read signal, query cancel, Job cancel, script cancel). Finished entries are kept for a configurable retention (30 s by default).
- Each command task runs under a panic guard; a panic fails the Operations and Jobs the task started and records one `engine_panic` Incident. A thread-local marker set around every poll lets the global panic hook leave those panics to the engine.
- Driver errors carry one of nine kinds (Connection, Auth, Syntax, Timeout, Cancelled, Permission, Unsupported, Protocol, Other), a message, an optional position and hint; the kind decides the UI presentation and the log level.
- Result chunks are columnar with physical kinds (bool, int, float, text, bytes, mixed) and display hints; validity bitmaps; offsets over one byte arena. Doc chunks encode documents as tagged values with varint lengths. The engine reads at most 1,024 rows per chunk.
- Bundled drivers are registered by the app at startup; the engine reaches them only through the driver interface.
- Performance harnesses: an app example that launches the release binary in temporary directories and measures startup, idle RSS, binary size and grid scrolling; an editor measurement example and a checker script for the editor budgets.
- A dependency check script enforces rustls only and a single egui version; CI runs formatting, Clippy with warnings denied, the dependency check and the workspace tests.

## Testing Decisions

- Engine seam: the public engine interface driven with the fake Drivers of the test kit (relational, document, key-value), covering ordering per Session, cancellation, panics converted to failed Operations, Operation retention. Prior art: the engine's operations, script, transaction and integration test suites.
- Driver seam: the Conformance suite (connect, ping, browse, execute and streaming, fetch more, affected rows, syntax errors, quoting round trip, cancel, type matrix, describe for editing) run by each relational Driver against the shared test services, and by sidecars through the protocol.
- UI seam: `egui_kittest` harnesses for widgets and for the whole app with fake Drivers; GPU visual audits are ignored tests run on demand.
- Fuzzing: the sidecar frame decoder and the chunk decoder.
- Shared services: one container per database on fixed ports, per-test databases, a cross-process test lock for server-global state, nextest groups that serialise lock-holding suites, and `SQLR_SKIP_CONTAINER_TESTS=1` to skip them.
- Performance: the budget harnesses on release builds, failing on any exceeded limit.

## Acceptance Criteria

- [x] Cold start to the first completed frame is under 1,000 ms in the budget harness.
- [x] Idle RSS with two connected SQLite databases is under 150 MB in the budget harness.
- [x] The stripped release binary is under 75 MB with every Bundled driver included.
- [x] Grid scrolling p99 is under 16.7 ms over 300 verified jumps on 1,000,000 rows.
- [x] Editor budgets (scenarios p99 under 8 ms, scope parsing p99 under 5 ms, first frame under 200 ms, peak RSS under 120 MB) are checked by the editor harness.
- [ ] CI runs the performance budgets on macOS and Linux and fails the build on a regression.
- [x] The UI thread performs no database, keychain, vault or connection-file I/O.
- [x] Every engine method returns without waiting for the database.
- [x] Every connect, browse, query, fetch, script and Job appears in the Operation registry and can be cancelled on its own.
- [x] Cancelling a connect ends it Cancelled at once and drops any tunnel it opened.
- [x] Finished Operations disappear after the configured retention.
- [x] A panic in an engine command task fails only its own Operations and records one `engine_panic` Incident.
- [x] A crashed Sidecar driver fails only its own Connections.
- [x] A Connection error fails its Session, closes its stream and reports a lost transaction.
- [x] Result chunks use the same bytes in process and over the sidecar protocol.
- [ ] Results that exceed their memory budget spill their oldest chunks to a temporary file read back through mmap.
- [ ] The engine throttles repaint requests to about 60 Hz.
- [x] The build contains no OpenSSL and exactly one egui version, enforced by a CI check.
- [x] UI widgets depend only on core and return actions instead of calling the engine.
- [x] The Conformance suite runs every relational Driver against a real database.
- [ ] The Conformance suite and other service-backed tests run in CI against the shared test services.
- [ ] Widget tests include snapshot comparisons for the query editor, result grid, schema tree and Key browser.
- [x] The sidecar frame decoder and the chunk decoder have fuzz targets.
- [x] `SQLR_SKIP_CONTAINER_TESTS=1` runs every test that needs no database service.

## Out of Scope

- Installers, code signing and notarization.
- Telemetry of any kind.
- Process sandboxing for third-party drivers.

## Further Notes

This task records retroactively the work that built the area as it exists today. The unticked criteria come from the earlier design documents and are not met by the code: CI currently runs only on Linux without container services and without the budget harnesses, memory budgets truncate instead of spilling, repaint is coalesced by egui rather than throttled by the engine, and widget tests use interaction checks plus on-demand visual audits instead of snapshot comparisons.
