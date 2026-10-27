---
status: done
---

# Local diagnostics, incidents and crash reporting

## Specs

- [Diagnostics](../specs/diagnostics.md)
- [Architecture](../specs/architecture.md)
- [Query execution](../specs/query-execution.md)
- [Drivers](../specs/drivers.md)
- [Agents and terminal](../specs/agents-and-terminal.md)
- [Settings and workspace](../specs/settings-and-workspace.md)
- [Results and editing](../specs/results-and-editing.md)

## Problem Statement

When something goes wrong in sqlr the person has no dependable way to find out what happened, and the maintainer has no dependable way to reconstruct the failure.

Expected failures appear only in the interface. A failed query shows in its result tab or a short toast, a failed Connection shows a banner, a failed background Job shows a Job error. None of these is written to the log, so once the tab closes or the toast expires the evidence is gone, and nothing records the Connection, driver, database, duration or statement that failed.

Unexpected failures are worse. A panic on the interface thread closes the application on macOS and the default message goes to a stderr that a person launching the app from Finder never sees. A panic in an engine command task is swallowed and leaves the Operation running forever. A sidecar driver crash fails its Connections without recording why. A failure while starting returns an error printed to stderr and nothing else: the application simply does not open.

The log that exists is a daily rotating file with no retention limit, no structured context, no session identifier and a level that changes only through `RUST_LOG` before launch. Nothing records an Incident, nothing announces a crash on the next launch, and there is no report to send.

## Solution

sqlr gains a local diagnostics system. It records structured logs for every failure, creates a self-contained Incident for every unexpected event, and keeps Incidents in the data directory under a bounded retention. sqlr sends nothing over the network and collects no telemetry; sharing a reviewed report is an explicit hand-off to the browser.

Every failed operation writes one structured log line when it settles, with the operation kind, Connection, driver, database, duration, error kind and, according to one setting, the SQL statement. Expected server errors stay log lines. Unexpected events become Incidents written as JSON files.

A global panic hook records the message, location, thread and App session identifier and writes a report before the process dies. Panics inside engine command tasks are caught and converted into failed Operations, so the interface never waits forever. Native crashes cannot be caught; a session lock and marker distinguish a clean exit from an unclean one and produce a lighter notice on the next launch.

On the next launch a non-blocking banner says the previous session ended abnormally, with Open report, Report on GitHub, Copy and Dismiss. The Diagnostics window from Help shows the current session summary and recent Incidents, and can copy, save or open a prefilled GitHub issue with a bounded bundle. Result errors gain Copy details, and Report a Problem records a manual Incident. Secrets never enter logs, Incidents or bundles; SQL is included by default and can be excluded with one setting shared by logs and reports.

## User Stories

1. As a person using sqlr, I want every failed query to write a log line with its Connection, driver, database, duration, error kind and statement, so that a failure can be reproduced later.
2. As a person using sqlr, I want unexpected failures to become Incidents automatically, so that I do not have to notice them and report them by hand.
3. As a person using sqlr, I want ordinary server errors such as syntax, permission, authentication, timeout and cancellation to stay out of the Incident list, so that the list contains only things worth investigating.
4. As a person using sqlr, I want a cancelled operation logged only at debug level, so that pressing Stop does not clutter the log.
5. As a person using sqlr, I want a failed Connection and a lost Connection to record the driver and the reason, so that network and authentication problems are diagnosable.
6. As a person using sqlr, I want a failed background Job to record its kind and outcome, so that a failed Job can be traced.
7. As a person using sqlr, I want a sidecar driver crash to record the driver identity and the reason the process ended, so that a driver problem is distinguishable from a query problem.
8. As a person using sqlr, I want an internal inconsistency, a decoding failure, a local database failure or a vault failure to become an Incident, so that application bugs are reported like crashes.
9. As a person using sqlr, I want a startup failure saved and shown in a native dialog, so that an application that does not open still explains itself.
10. As a person using sqlr, I want every App session to have an identifier that appears in logs and Incidents, so that a report can be matched with the log of the run that produced it.
11. As a person using sqlr, I want Connection, disconnection, Session and sidecar lifecycle events in the log, so that I can reconstruct what the application was doing.
12. As a person using sqlr, I want the application not to record values returned by the server or parameter values, so that log lines stay small and safe.
13. As a person using sqlr, I want a panic on any thread to write a crash report before the process dies, so that a crash does not disappear.
14. As a person using sqlr, I want a panic in an engine command task to fail the corresponding Operation instead of leaving it running forever, so that the interface reflects reality.
15. As a person using sqlr, I want a panic in the interface thread to let the application exit rather than continue with corrupted state, so that I do not lose data to a half-broken process.
16. As a person using sqlr, I want a panic in a support thread to be recorded, and the operation that depended on it to fail rather than wait forever.
17. As a person using sqlr, I want the next launch to show a non-blocking banner when the previous session ended with a panic, so that I notice the crash without being blocked from working.
18. As a person using sqlr, I want a lighter notice when the previous session ended uncleanly without a recorded panic, so that the application does not claim to know a cause it does not know.
19. As a person using sqlr, I want Open report, Report on GitHub, Copy and Dismiss on that banner, so that I can act immediately.
20. As a person using sqlr, I want Dismiss to be remembered, so that the same crash is not announced at every launch.
21. As a person using sqlr, I want a clean exit to remove the session marker, so that a normal quit never produces a false crash notice.
22. As a person using sqlr, I want a second running instance not to make the first one look crashed, so that multiple instances with the same data directory do not produce false alarms.
23. As a person using sqlr, I want a segmentation fault or an out-of-memory kill detected as an unclean shutdown even though no report exists, so that I know the session did not end normally.
24. As a person using sqlr, I want the last known log lines included in a crash report, so that the events immediately before the crash are not lost when the file writer has not flushed.
25. As a person using sqlr, I want a Diagnostics window opened from Help, so that I can find the feature without knowing where files live.
26. As a person using sqlr, I want the window to show the current session summary with version, build, system, drivers, Connections and open work, so that I can see what the application knows about itself.
27. As a person using sqlr, I want the window to list recent Incidents with their time, kind and message, so that I can choose the relevant one.
28. As a person using sqlr, I want to open one Incident and read its event, environment and recent log lines, so that I understand what happened before sharing it.
29. As a person using sqlr, I want Copy diagnostics to put the current bundle on the clipboard, so that I can paste it into an issue or a chat.
30. As a person using sqlr, I want Save bundle… to write the bundle to a file, so that I can attach it to a report.
31. As a person using sqlr, I want an explicit preview of what the bundle contains before it is copied, saved or opened as a GitHub issue, so that I never share more than I intend to.
32. As a person using sqlr, I want Open GitHub issue to prefill a public issue with the whole report, inline when it fits the URL and otherwise copied for one paste, so that a report carries all the information with minimal effort.
33. As a person using sqlr, I want Copy details in a result's error panel, so that I can copy the error with its Connection, database, statement and duration without opening the Diagnostics window.
34. As a person using sqlr, I want Report a Problem… with a free-text note, so that I can explain what I was doing when the problem happened.
35. As a person using sqlr, I want to choose whether the manual report includes SQL and open tab titles, so that I control sensitive content.
36. As a person using sqlr, I want a manual report to appear in the Incident list, so that it can be reopened and shared later like an automatic one.
37. As a person using sqlr, I want bundle files to have a predictable, bounded size with explicit truncation markers, so that they are easy to attach and do not grow without limit.
38. As a person using sqlr, I want absolute paths in a bundle shortened to `<home>/…` and `<data-dir>/…`, so that the report does not expose my user name unnecessarily.
39. As a person using sqlr, I want diagnostics to make no network requests and sharing to happen only through an explicit browser hand-off, so that the application keeps its no-telemetry promise.
40. As a security-conscious person, I want passwords, passphrases, bearer tokens and vault contents never to appear in logs, Incidents or bundles, so that sharing a report is safe.
41. As a person working with regulated data, I want one setting that excludes SQL from both logs and reports, so that I do not have to remember two switches.
42. As a person using sqlr, I want SQL included by default, so that a report is useful without extra configuration.
43. As a person using sqlr, I want failed SQL truncated with a clear marker, so that one huge statement does not dominate the log or the bundle.
44. As a person using sqlr, I want to change the log level for the current App session from the Diagnostics window, so that I can capture more detail without relaunching from a terminal.
45. As a person using sqlr, I want the log level to reset to the default on the next launch, so that a debugging session does not silently leave the application logging at debug forever.
46. As an advanced person, I want `RUST_LOG` to override the in-app level, so that my existing launch scripts keep working.
47. As a person using sqlr, I want the logs and diagnostics folders to be readable only by me, so that SQL and Connection details are not exposed to other local users.
48. As a person using sqlr, I want old log files and old Incidents pruned automatically, so that the application does not fill the disk.
49. As a person using sqlr, I want Open logs folder in the Diagnostics window, so that I can inspect or archive the raw logs myself.
50. As a person using sqlr, I want to be told in the interface when the log folder is not writable, so that I am not under the illusion that diagnostics are being saved.
51. As a person maintaining sqlr, I want operation spans with operation, Connection, driver, database and query identifiers, so that concurrent activity can be told apart.
52. As a person maintaining sqlr, I want one failure line per Operation at the point it settles, so that the log does not repeat the same error at every layer.
53. As a person maintaining sqlr, I want sidecar driver output tagged with a driver field, so that logs from several drivers can be filtered.
54. As a person maintaining sqlr, I want log files rotated daily and bounded to fourteen files, so that disk use is predictable.
55. As a person maintaining sqlr, I want the `error!`-means-Incident rule documented as a hard rule, so that future error paths do not silently lose Incidents.
56. As a person maintaining sqlr, I want a session identifier at startup with version, system, data directory and driver count, so that a log file is self-describing.
57. As a person using sqlr, I want a sidecar driver crash to fail only that driver's Connections and leave the rest of the application working.
58. As a person using sqlr, I want one engine task panic not to stop other Operations, so that unrelated work keeps running.
59. As a person using sqlr, I want the Diagnostics window to work even when the local SQLite database, the vault or the configuration is broken, so that the worst failures are still reportable.
60. As a person using sqlr on macOS, I want the startup-failure dialog to be a native dialog, so that it appears even when the application never opens a window.
61. As a person using sqlr, I want diagnostics files to be ordinary JSON, so that I can read them with any tool and they survive outside the application.
62. As a person using sqlr, I want the startup notice to name only the clean or unclean outcome when no panic was recorded, so that it does not guess a cause.

## Implementation Decisions

- The diagnostics system lives in the application and introduces no new workspace component. The engine, the sidecar client and the application emit structured tracing events they already produce; no diagnostics dependency is added to the engine or sidecar client.
- The ingestion contract is the logging contract: an event at error level is an Incident and every other level is a log line. An optional `incident` field selects the Incident kind; without it the kind is `error`. Span fields are inherited so Operation context needs no fixed schema.
- A tracing layer writes every event to the process-wide ring, applies the failure-level policy and queues error events to the diagnostics writer. The queue keeps the logging thread from blocking on file I/O.
- The panic hook is installed as early as the data directory is known and before logging and the engine. It records payload, source location, thread name, App session identifier and the ring excerpt, writes directly with plain file operations, never takes application locks, chains to the previous hook, and detects its own re-entry.
- Engine command tasks run inside an engine-task scope so the global hook leaves their panics to the engine converter, which fails the query or document result, the Jobs and the Operations, and records one `engine_panic` Incident with operation context. Other commands continue.
- The Incident model has schema version 1 with kind, message, target, thread, location, error kind, free-form fields, note, acknowledged flag, environment, log excerpt and reserved backtrace. Unknown fields survive a read.
- The Incident file store writes one JSON file per Incident, atomically through a temporary file and rename, with owner-only file and folder permissions, validates file names against the identifier scheme, prunes to twenty files or thirty days at startup and after each write, and never depends on SQLite, the vault or the configuration.
- The session lock and marker are separate files. The lock is an exclusive non-blocking lock held for the App session; the marker is written atomically with the App session id, process id, start time and version, removed on a clean exit and kept when a panic was recorded.
- At startup the app reads the previous marker under the lock. An unacknowledged panic Incident for that App session produces a full crash notice; otherwise the app records an `unclean_shutdown` Incident and shows the lighter notice. A second instance holding no lock reports nothing and leaves the marker alone.
- The startup notice is a non-blocking overlay with Open report, Report on GitHub, Copy and Dismiss. Copy uses the Incident JSON exactly as written; Dismiss acknowledges the Incident and hides the notice.
- Failures after the data directory is known are recorded as `startup` Incidents and shown in a native blocking dialog with the message, Copy details and Open logs before the process exits non-zero. Argument parsing errors stay stderr errors with exit code 2.
- The log ring is a bounded in-memory queue of 500 lines and 256 KiB, caps one line at 8 KiB, drops the oldest entries first and records dropped line and byte counts for the excerpt marker.
- Logging writes to stderr and to a daily file through a non-blocking writer. Each line carries the App session identifier. A reloadable filter holds the session-only level, `RUST_LOG` wins when present, and changing the level keeps the MCP protocol library at warn.
- The failure-line policy maps error kinds to levels: syntax, permission, auth and timeout to info; cancelled to debug; connection, protocol, unsupported and other to warn. One line is written where the outcome is final.
- The SQL setting is a global persisted boolean, default on, applied to the tracing layer, Incident creation, ring excerpts and bundles, and to the engine. SQL is truncated at 4 KiB on a character boundary with a marker and always written last so it can be removed later.
- The bundle builder takes cheap snapshots from the interface thread, then renders on the diagnostics writer: section caps, string caps, sensitive-setting omission, per-report SQL and title choices, path shortening, sentinel redaction, a 512 KiB total limit and explicit truncation markers. Persistent section shrinking and a last-resort minimal document keep the JSON valid and bounded.
- The GitHub issue builder titles the issue from the Incident kind and first message line, builds a short summary body, embeds the full bundle in a collapsible block when the percent-encoded URL fits a 6 KiB budget, and otherwise copies the bundle and leaves a paste marker. The browser opens only after review.
- The Diagnostics window is a non-modal viewer with the current session summary, the Incident list and details, the log level and logs folder action, and the SQL privacy checkbox. Bundle work runs on the diagnostics writer and the preview appears when it is ready.
- Copy details in a result error panel builds text from the error, hint, Connection, database, position, statement subject to the SQL setting and elapsed time, and records no Incident.
- Report a Problem records a `manual` Incident with the note, the first line as message, and the SQL and title choices; the same review path handles copy, save and GitHub issue.
- Environment collection runs on a background thread once per App session and once per bundle, omits values a platform cannot provide, and includes the application version, build profile, target triple, commit, operating system, architecture, OS version, locale, graphics backend and resident memory.
- Engine and sidecar instrumentation attaches operation context at the command boundary, logs sidecar process exit, handshake failure, protocol violations and kill reasons with the driver identity, and forwards driver output with a driver field.

## Testing Decisions

- **Diagnostics value seam**: the highest seam. Record, list, read, acknowledge and prune Incidents; build a bundle; and exercise the session marker and lock lifecycle against a temporary data directory. It covers serialization round trips, retention, acknowledgement, clean and unclean exits, second-instance detection, ring bounds, SQL omission, bundle caps, path shortening and sentinel redaction. Prior art: the store tests that use temporary directories.
- **Panic hook seam**: install the hook, panic on a named thread inside `catch_unwind`, restore the previous hook and assert the written Incident. Prior art: the hook tests, with a serialising lock because the hook is global; nextest runs each test in its own process.
- **Logging layer seam**: a custom subscriber with an injected ring asserts that error events become Incidents of the named kind, info events do not, span fields are inherited, the SQL setting removes SQL from Incidents and the ring, and SQL truncates at a character boundary. Prior art: the logging tests.
- **Engine seam**: a panicking fake driver makes the query and its Operation fail while the engine keeps running and a second query succeeds. Prior art: the engine integration tests with the fake driver.
- **Application UI seam** with egui_kittest: the startup banner's panic and unclean wordings, Dismiss persistence, the Diagnostics window opening from Help, Incident selection and log excerpt, the SQL toggle, the review window, and Copy details in a result error panel. Prior art: the existing application harness tests.
- **Filesystem seam**: owner-only permissions, atomic rewriting and retention on real files in a temporary directory, plus the unwritable-log-folder warning. Prior art: the store's file tests.
- **Sidecar seam**: the engine and sidecar tests keep covering process death, handshake failure and protocol violations, and the diagnostics contract is asserted at the event level; no test starts a real sidecar process only to assert an Incident.
- A secret sentinel test builds a bundle from settings and Connections containing sentinel passwords and tokens and asserts that no sentinel appears anywhere in the output.

## Acceptance Criteria

- [x] A failed query writes exactly one log line with operation, Connection, driver, database, elapsed time, error kind and, when enabled, the statement.
- [x] A syntax, permission, authentication, timeout or cancelled operation creates no Incident.
- [x] An unexpected error at error level creates an Incident file in the diagnostics folder.
- [x] A panic on the interface thread writes a crash report containing message, file, line, thread, App session identifier and recent log lines, and the next launch shows the banner.
- [x] A panic in an engine command ends its Operation as failed with an internal-panic message, writes an `engine_panic` Incident, and leaves other Operations working.
- [x] A sidecar driver exit records the driver identity and reason, fails only its Connections, and does not stop the application.
- [x] A failure after the data directory is known writes a `startup` Incident and shows a native dialog before exiting non-zero.
- [x] A session that is killed or loses power produces the lighter unclean-shutdown notice on the next launch; a clean exit produces none.
- [x] Dismissing a startup notice prevents it from appearing again for that Incident.
- [x] The Diagnostics window opens from Help, shows the session summary and recent Incidents, and can copy and save a bundle.
- [x] A saved bundle contains the documented sections, stays within 512 KiB, and marks every truncated section.
- [x] With `diagnostics.include_sql` off, no SQL appears in logs, Incidents or bundles; with it on, failed SQL appears and is truncated at 4 KiB with a marker.
- [x] No password, passphrase, bearer token or vault value appears in logs, Incidents or bundles, verified by sentinel values.
- [x] A manual report records its note, appears in the Incident list, and honours the SQL and tab-title choices.
- [x] The log level can be changed for the App session from the Diagnostics window, resets on the next launch, and `RUST_LOG` takes precedence when set.
- [x] Log files rotate daily, at most fourteen are kept, and Incidents are pruned to twenty files or thirty days.
- [x] The logs and diagnostics folders are owner-only, and Incident files are owner-only.
- [x] If the log folder is unwritable, the application starts and shows a warning that diagnostics are limited to stderr.
- [x] No diagnostics code performs a network request.
- [x] The workspace tests, clippy and fmt pass.

## Out of Scope

- Remote reporting, crash upload, telemetry, an installation identifier or any network endpoint.
- Backtraces, symbolication, dSYM or other debug artifacts, build-identifier capture and address-to-symbol tooling. Release builds stay stripped and crash reports contain no stack trace.
- Minidumps, native signal handlers or operating-system crash-reporter integration. Segmentation faults and out-of-memory kills are detected only as an unclean shutdown.
- Automatic SQL literal redaction or masking. SQL is recorded as typed when enabled.
- Size-based log rotation, compression or archiving.
- Per-profile or per-Connection diagnostics settings; the SQL setting is global.
- Exposing diagnostics, Incidents or logs through MCP or the Agent API.
- Changing what History stores or retroactively removing SQL from existing History.
- Changing the vault, keychain or secret-storage behaviour.
- Recovering from an interface-thread panic and continuing to run.
- Retroactive capture of failures that happened before this system existed.
- Application update checks or any unrelated networking.

## Further Notes

This task records retroactively the work that built the area as it exists today. The known limitation that a crash report cannot carry a live snapshot of Connections, tabs and Operations is deliberate: the panic hook must not take application locks. Those sections appear in a bundle built from the Diagnostics window and are labelled as the current session.
