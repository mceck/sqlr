# Diagnostics, errors and crash reporting

## Problem Statement

When something goes wrong in sqlr, the person using it has no dependable way to find out
what happened, and the person maintaining it has no dependable way to reconstruct the
failure.

Expected failures are only shown in the interface. A failed query appears in its result
tab or in a short-lived toast, a failed connection shows a banner, a failed background job
shows a job error. None of these is written to the log, so once the tab is closed or the
toast expires the evidence is gone, and nothing records the connection, the driver, the
database, the duration or the statement that failed.

Unexpected failures are worse. A panic on the interface thread closes the application on
macOS — winit stops the event loop — and the default panic message goes to a stderr that a
person who launched the `.app` from Finder never sees. A panic inside an engine command
task is swallowed by the dropped `JoinHandle`: the operation stays *running* forever, with
no error and no log. A sidecar driver crash fails its connections but does not record why
the process died, and the forwarded driver output is the only trace. A failure while
starting — an unreadable settings file, an unavailable vault, a driver that cannot be
loaded, a graphics backend that cannot initialise — returns an error that is printed to
stderr and nothing else: the application simply does not open.

The log that does exist is a daily rotating file with no retention limit, no structured
context, no session identifier, and a level that can only be changed through `RUST_LOG`
before launch. Diagnostics never depend on the local SQLite database, the vault or the
configuration, but no such diagnostics exist: the design document promises a "Copy
diagnostics" action and that action has never been implemented. The release binary is
stripped and has no symbols on purpose, so any backtrace would be a list of addresses.

The result is a support loop made of guesses: the person cannot tell whether the problem
was their query, the network, a driver or the application; the maintainer cannot ask for
"the report" because there is no report to send.

## Solution

sqlr gains a **local diagnostics system**. It records structured logs for every failure,
creates a self-contained **incident** for every unexpected event, and keeps those
incidents in the data directory with a bounded retention. sqlr sends nothing over the
network and there is no telemetry; sharing a reviewed report is an explicit hand-off to
the browser.

Every failed operation writes one structured log line at the moment it settles, with the
operation kind, connection, driver, database, duration, error kind and — according to a
single user setting — the SQL statement that failed. Expected server errors (syntax,
permission, authentication, timeout, cancellation) remain log lines. Unexpected events
(internal errors, protocol errors, driver crashes, lost connections, startup failures,
panics) become incidents and are written as JSON files.

A **global panic hook** records the panic message, location, thread and session identifier
and writes a crash report before the process dies. Panics inside engine command tasks are
caught and converted into failed operations, so the interface never waits forever for
work that will not finish. Native crashes (segmentation faults, out-of-memory kills) cannot
be caught; a session lock and marker distinguish a clean exit from an unclean one and
produce a lighter notice on the next launch.

On the next launch, a non-blocking banner says that the previous session ended
abnormally, with **Open report**, **Report on GitHub**, **Copy** and **Dismiss**. A new
**Diagnostics** window, opened from Help, shows the current session summary and the recent
incidents, and can **copy**, **save** or **open as a prefilled GitHub issue** a
diagnostics bundle that contains the application and system environment, driver versions,
sanitized settings, connection metadata, interface state, recent operations, the recent
log ring and the incident itself. The issue action hands the report to the browser's
public new-issue page after review; sqlr itself makes no request. The window also exposes
a session-only log level, the logs folder and the SQL setting.

The interface surfaces the same information where failures happen: a result's error panel
gains **Copy details**. A **Report a Problem** dialog lets the person add a note and
choose what to include before copying or saving; it records a manual incident so the
report appears in the list like any other.

Secrets — passwords, passphrases, tokens, vault contents — never enter logs, incidents or
bundles, as today. SQL is included by default because it is usually the only way to
reproduce a failure, and it can be excluded with one setting shared by logs and reports.
There is no automatic SQL-literal redaction: SQL is recorded as typed, and the person
reviews the bundle before sharing it.

## User Stories

### Capturing failures

1. As a person using sqlr, I want every failed query to write a log line with its
   connection, driver, database, duration, error kind and statement, so that a failure can
   be reproduced later.
2. As a person using sqlr, I want unexpected failures to become incidents automatically,
   so that I do not have to notice them and report them by hand.
3. As a person using sqlr, I want ordinary server errors — syntax, permission,
   authentication, timeout and cancellation — to stay out of the incident list, so that
   the list contains only things worth investigating.
4. As a person using sqlr, I want a cancelled operation to be logged only at debug level,
   so that pressing Stop does not clutter the log.
5. As a person using sqlr, I want a failed connection and a lost connection to record the
   driver and the reason, so that network and authentication problems are diagnosable.
6. As a person using sqlr, I want a failed background job — apply, import, export,
   transfer, dump, restore, administration — to record its kind and outcome, so that a
   failed job can be traced.
7. As a person using sqlr, I want a sidecar driver crash to record the driver identity and
   the reason the process ended, so that a driver problem is distinguishable from a query
   problem.
8. As a person using sqlr, I want an internal inconsistency, a decoding failure, a local
   database failure or a vault/keychain failure to become an incident, so that application
   bugs are reported like crashes.
9. As a person using sqlr, I want a startup failure to be saved and shown in a native
   dialog, so that an application that does not open still explains itself.
10. As a person using sqlr, I want every session to have an identifier that appears in
    logs and incidents, so that a report can be matched with the log of the run that
    produced it.
11. As a person using sqlr, I want connection, disconnection, session and sidecar
    lifecycle events in the log, so that I can reconstruct what the application was doing.
12. As a person using sqlr, I want the application not to record values returned by the
    server or parameter values in logs, so that log lines stay small and safe.

### Crashes and restarts

13. As a person using sqlr, I want a panic on any thread to write a crash report before the
    process dies, so that a crash does not disappear.
14. As a person using sqlr, I want a panic in an engine command task to fail the
    corresponding operation instead of leaving it running forever, so that the interface
    reflects reality.
15. As a person using sqlr, I want a panic in the interface thread to let the application
    exit rather than continue with corrupted state, so that I do not lose data to a
    half-broken process.
16. As a person using sqlr, I want a panic in a support thread (store, agents, terminal,
    fonts) to be recorded, and the operation that depended on it to fail rather than wait
    forever.
17. As a person using sqlr, I want the next launch to show a non-blocking banner when the
    previous session ended with a panic, so that I notice the crash without being blocked
    from working.
18. As a person using sqlr, I want a lighter notice when the previous session ended
    uncleanly without a recorded panic (kill, out-of-memory, power loss), so that the
    application does not claim to know a cause it does not know.
19. As a person using sqlr, I want **Open report**, **Report on GitHub**, **Copy** and
    **Dismiss** on that banner, so that I can act immediately.
20. As a person using sqlr, I want **Dismiss** to be remembered, so that the same crash is
    not announced at every launch.
21. As a person using sqlr, I want a clean exit to remove the session marker, so that a
    normal quit never produces a false crash notice.
22. As a person using sqlr, I want a second running instance not to make the first one look
    crashed, so that multiple instances with the same data directory do not produce false
    alarms.
23. As a person using sqlr, I want a segmentation fault or an out-of-memory kill to be
    detected as an unclean shutdown even though no report exists, so that I know the
    session did not end normally.
24. As a person using sqlr, I want the last known log lines to be included in a crash
    report, so that the events immediately before the crash are not lost when the file
    writer has not flushed.

### Reviewing and sharing

25. As a person using sqlr, I want a Diagnostics window opened from Help, so that I can
    find the feature without knowing where files live.
26. As a person using sqlr, I want the Diagnostics window to show the current session
    summary — version, build, system, drivers, connections and open work — so that I can
    see what the application knows about itself.
27. As a person using sqlr, I want the Diagnostics window to list recent incidents with
    their time, kind and message, so that I can choose the relevant one.
28. As a person using sqlr, I want to open one incident and read its event, environment and
    recent log lines, so that I can understand what happened before sharing it.
29. As a person using sqlr, I want **Copy diagnostics** to put the current bundle on the
    clipboard, so that I can paste it into an issue or a chat.
30. As a person using sqlr, I want **Save bundle…** to write the bundle to a file, so that
    I can attach it to a report.
31. As a person using sqlr, I want an explicit preview of what the bundle contains before
    it is copied, saved or opened as a GitHub issue, so that I never share more than I
    intend to.
32. As a person using sqlr, I want **Open GitHub issue** to prefill a public issue with
    the whole report — inline when it fits the URL, otherwise copied for one paste — so
    that a report carries all the information with minimal effort.
33. As a person using sqlr, I want **Copy details** in a result's error panel, so that I
    can copy the error with its connection, database, statement and duration without
    opening the Diagnostics window.
34. As a person using sqlr, I want **Report a Problem…** with a free-text note, so that I
    can explain what I was doing when the problem happened.
35. As a person using sqlr, I want to choose whether the manual report includes SQL and
    open tab titles, so that I control sensitive content.
36. As a person using sqlr, I want a manual report to appear in the incident list, so that
    it can be reopened and shared later like an automatic one.
37. As a person using sqlr, I want bundle files to have a predictable, bounded size with
    explicit truncation markers, so that they are easy to attach and do not grow without
    limit.
38. As a person using sqlr, I want absolute paths in a bundle shortened to
    `<home>/…` and `<data-dir>/…`, so that the report does not expose my user name
    unnecessarily.

### Privacy and control

39. As a person using sqlr, I want diagnostics to make no network requests and sharing to
    happen only through an explicit browser hand-off, so that the application keeps its
    no-telemetry promise.
40. As a security-conscious person, I want passwords, passphrases, bearer tokens and vault
    contents never to appear in logs, incidents or bundles, so that sharing a report is
    safe.
41. As a person working with regulated data, I want one setting that excludes SQL from
    both logs and reports, so that I do not have to remember two switches.
42. As a person using sqlr, I want SQL to be included by default, so that a report is
    useful without extra configuration.
43. As a person using sqlr, I want failed SQL to be truncated with a clear marker, so that
    one huge statement does not dominate the log or the bundle.
44. As a person using sqlr, I want to change the log level for the current session from the
    Diagnostics window, so that I can capture more detail without relaunching from a
    terminal.
45. As a person using sqlr, I want the log level to reset to the default on the next
    launch, so that a debugging session does not silently leave the application logging at
    debug forever.
46. As an advanced person, I want `RUST_LOG` to override the in-app level, so that my
    existing launch scripts keep working.
47. As a person using sqlr, I want the logs and diagnostics directories to be readable
    only by me, so that SQL and connection details are not exposed to other local users.
48. As a person using sqlr, I want old log files and old incidents to be pruned
    automatically, so that the application does not fill the disk.
49. As a person using sqlr, I want **Open logs folder** in the Diagnostics window, so that
    I can inspect or archive the raw logs myself.
50. As a person using sqlr, I want to be told in the interface when the log directory is
    not writable, so that I am not under the illusion that diagnostics are being saved.

### Logging and observability

51. As a person maintaining sqlr, I want operation spans with operation, connection,
    driver, database and query identifiers, so that concurrent activity can be told apart.
52. As a person maintaining sqlr, I want one failure line per operation at the point it
    settles, so that the log does not repeat the same error at every layer.
53. As a person maintaining sqlr, I want sidecar driver output tagged with a driver field,
    so that logs from several drivers can be filtered.
54. As a person maintaining sqlr, I want log files rotated daily and bounded to fourteen
    files, so that disk use is predictable.
55. As a person maintaining sqlr, I want the `error!`-means-incident rule documented next
    to the other hard rules, so that future error paths do not silently lose incidents.
56. As a person maintaining sqlr, I want a session identifier at startup with version,
    system, data directory and driver count, so that a log file is self-describing.

### Reliability and platforms

57. As a person using sqlr, I want a sidecar driver crash to fail only that driver's
    connections and leave the rest of the application working.
58. As a person using sqlr, I want one engine task panic not to stop other operations, so
    that unrelated work keeps running.
59. As a person using sqlr, I want the Diagnostics window to work even when the local
    SQLite database, the vault or the configuration is broken, so that the worst failures
    are still reportable.
60. As a person using sqlr on macOS, I want the startup-failure dialog to be a native
    dialog, so that it appears even when the application never opens a window.
61. As a person using sqlr, I want the diagnostics files to be ordinary JSON, so that I
    can read them with any tool and they survive outside the application.

## Implementation Decisions

### Workspace and module boundaries

The diagnostics system lives in the application crate. It does not introduce a new
workspace crate and does not add a diagnostics dependency to the engine or sidecar crates.

The **ingestion contract** is the logging contract: an `error!` event is an incident, and
every other level is a log line. The engine, the sidecar client and the application emit
structured events with `tracing`, which they already depend on. The application installs a
tracing layer that turns error-level events into incidents and sends them to a dedicated
diagnostics writer thread. The panic hook is the only producer that writes an incident
directly, because a crashing process cannot rely on a background thread. This contract is
recorded as a hard rule in the project instructions: `error!` is reserved for unexpected
events and always produces an incident; expected server errors use `info` or `warn`.

The diagnostics module owns the incident model, the file store, the session marker and
lock, the log ring, the panic hook, the bundle builder and the Diagnostics window. The
application holds one initialized diagnostics value and passes read-only snapshots to it.
The engine and sidecar remain unaware of files, JSON and UI.

### Incident model

An incident is a JSON document with a stable schema version. Kinds are:

- `panic` — a panic on any thread.
- `startup` — a failure before or during application startup.
- `unclean_shutdown` — the previous session ended without a clean exit and without a
  recorded panic.
- `engine_panic` — a panic inside an engine command task, with the operation context.
- `sidecar_crash` — a driver process exited or broke the protocol.
- `error` — an unexpected application-level error emitted at error level.
- `manual` — a report created by the person through Report a Problem.

The incident shape (schema version 1) is:

```json
{
  "schema": 1,
  "id": "2026-10-24T20-15-03Z-4711-a1b2",
  "session_id": "6f0c…",
  "at_ms": 1790000000000,
  "kind": "panic",
  "message": "assertion failed: …",
  "target": "sqlr_app::diagnostics",
  "thread": "main",
  "file": "…",
  "line": 812,
  "error_kind": null,
  "fields": {"op": "query", "conn": "c1", "driver": "postgres"},
  "note": null,
  "acknowledged": false,
  "environment": {"version": "0.1.0", "os": "macos", "arch": "aarch64"},
  "logs": ["…recent lines…"]
}
```

`error_kind` reuses the shared driver error kinds when the event comes from a driver
error. `fields` carries operation context without a fixed schema, so new instrumentation
does not require a schema change. `note` is set only for manual reports. The incident
identifier is derived from the UTC timestamp, the process identifier and a short random
suffix, so two processes writing at the same instant cannot collide.

Incident files contain the event, the environment summary and the log ring excerpt. They
do not contain a live snapshot of connections, tabs or operations: the panic hook must not
take application locks, and the tracing layer runs on arbitrary threads. The current
session snapshot is added when a bundle is produced from the Diagnostics window and is
labelled as the current session, never as the state at the time of the incident.

### Incident storage

Incidents live in a `diagnostics` directory under the data directory, one JSON file per
incident, named from the incident identifier. Writes are atomic through a private
temporary file in the same directory followed by a rename, and files are created with
owner-only permissions; the directory is owner-only as well. The store never depends on
the local SQLite database, the vault or the configuration, so a broken database or
configuration cannot prevent a report.

Retention is twenty incident files or thirty days, whichever limit is reached first.
Pruning runs at startup and after each new incident, ignores files that do not match the
naming scheme, and never fails startup.

Acknowledging an incident rewrites its `acknowledged` field atomically. Acknowledged
incidents stay available in the list but are not announced on startup.

### Log ring

A tracing layer keeps the most recent 500 log lines, capped at 256 KiB, in memory. The
ring is bounded by both counts and bytes, drops the oldest entries first, and is safe to
read from the panic hook. It exists so that a crash report contains the lines
immediately before the crash even when the asynchronous file writer has not flushed them.
The ring is not persisted separately; the bundle and incident files embed its excerpt with
an explicit truncation marker when entries were dropped.

### Panic capture and operation failure

The panic hook is installed as early as the data directory is known, before logging and
before the engine starts. It records:

- the panic payload rendered as text;
- the source location (file and line);
- the current thread name, or a generic label when unnamed;
- the session identifier;
- the log ring excerpt.

It writes the incident best-effort with plain file operations, never takes application
locks, and then invokes the previously installed hook so the standard stderr output is
preserved. If the diagnostics directory cannot be written, it falls back to stderr and
does not panic again.

Engine command tasks are spawned per command. Each task is wrapped so that a panic is
caught, converted to a text message, and recorded as an `engine_panic` incident at error
level with the operation kind, operation identifier and connection when known. The
corresponding operation ends as `Failed` with an internal-panic message; other operations
and the engine loop continue. This replaces the current behaviour where the dropped task
handle leaves the operation running forever.

Support threads keep the global hook for recording. The store and agents paths additionally
fail the operation that was waiting when the worker disappears, instead of leaving it
pending for the rest of the session. The interface thread is deliberately not wrapped in a
per-frame `catch_unwind`: state may be inconsistent, and continuing would risk writing
over good data. On macOS the event loop stops and the process exits; the report is written
by the hook and announced on the next launch.

### Clean and unclean shutdown

At startup the application opens a session lock file in the diagnostics directory and
takes an exclusive, non-blocking lock on it using the standard library's file locking. The
same file carries the current session marker, written atomically. A second instance that
finds the lock held treats the first instance as alive and does not report a crash.

On a clean exit the marker is removed and the lock is released. On the next startup, if
the lock can be acquired but a marker exists, the previous session ended uncleanly. If the
marker names a panic incident from that session, the startup banner offers the full crash
report; otherwise it shows the lighter unclean-shutdown notice. The marker contains the
session identifier, process identifier, start time and application version.

### Bundle contents and limits

A bundle is one JSON document produced on demand. It contains:

- **Application**: version, build profile, target triple, short commit hash when the build
  embeds one, and the graphics backend when eframe reports it.
- **System**: operating system and best-effort version, architecture, locale, session
  uptime, process identifier and resident memory when available.
- **Drivers**: identifier, version, whether bundled or installed, and sidecar state
  including restart count.
- **Settings**: the resolved global settings and any stored overrides, sanitized, with
  secret values never present.
- **Connections**: identifier, display name, driver, host, port, database, user, TLS mode
  and profile; session status and open transaction state. Secret field values are never
  collected.
- **Interface state**: open tab count, titles, connection and database, and current jobs
  and operations.
- **Operations**: the last fifty operations with kind, connection, label, duration,
  outcome and error.
- **Logs**: the ring excerpt.
- **Incidents**: the selected incident in full, or the recent incident summaries when no
  incident is selected.

The total bundle is capped at 512 KiB. Each list has its own smaller cap, and every
truncated section carries a marker naming what was omitted. Absolute paths under the home
directory and the data directory are shortened to `<home>/…` and `<data-dir>/…`.

The snapshot is collected as cheap copies on the interface thread and serialized and
written on the diagnostics writer thread; the interface thread never performs file I/O.
Copying to the clipboard happens on the interface thread once the serialized text is
available. Saving uses the native save dialog.

Incident files are also valid standalone reports. Copying from a crash banner copies the
incident JSON as written; saving from the Diagnostics window saves a bundle.

### Logging

Logging keeps stderr plus a daily rotating file, and adds the in-memory ring. It carries a
session identifier as a field on every file and stderr line, so that lines can be
correlated across reports even when several log files exist.

Operation failures produce exactly one line at the point they settle:

- query and fetch settlement;
- job settlement;
- connect attempts and connection loss;
- sidecar process exit and protocol failure;
- startup and configuration failures.

The fields are `op`, `conn`, `driver`, `operation_id`, `query_id`, `database`,
`elapsed_ms`, `error_kind` and `sql` (subject to the SQL setting). Expected error kinds
(`Syntax`, `Permission`, `Auth`, `Timeout`) log at `info`; unexpected kinds
(`Connection`, `Protocol`, `Unsupported`, `Other`) log at `warn`; `Cancelled` logs at
`debug`.

Spans exist only at operation boundaries in the engine: connect, query, fetch, script,
job, administration, key-value and document work, and MCP tool calls. There is no
instrumentation of interface-thread functions or functions called once per frame. Sidecar
driver output is forwarded with a `driver` field instead of a bracketed identifier inside
the message.

Lifecycle events log at `info`: startup with version, system, data directory, driver count
and session identifier; successful and failed connects; disconnects; session open and
close; sidecar start, stop and unexpected exit.

Server values, parameter values, secrets and raw result rows are never logged.

### SQL in logs and reports

A single global setting, `diagnostics.include_sql`, defaults to true and controls both
logs and reports. When false, SQL is omitted entirely from failure lines, incidents and
bundles. When true, SQL is recorded as typed, truncated to 4 KiB of UTF-8 at a character
boundary with an explicit truncation marker. No automatic literal redaction is performed;
the existing rule that secrets never enter logs still applies, and the person reviews the
bundle before sharing it. A targeted password-pattern redaction is intentionally out of
scope.

### Log level and retention

The log level is configurable from the Diagnostics window (`error`, `warn`, `info`,
`debug`), applies to the running session only, and is not persisted. The default is
`info`, with the existing `rmcp=warn` suppression. If `RUST_LOG` is set, it wins and the
window indicates that the environment controls the level.

Log files rotate daily and at most fourteen are kept. The logs directory and the
diagnostics directory are owner-only. If the log directory cannot be created or written,
the application still starts and shows a persistent warning that diagnostics are limited
to stderr; the startup itself is never blocked by diagnostics.

### Startup failures

Argument parsing errors stay command-line errors on stderr with a non-zero exit code. Once
the data directory is known, the panic hook and diagnostics are initialized. Any failure
after that — settings, connections, vault, keychain, driver registry, engine, graphics
backend, eframe startup — is written as a `startup` incident and shown in a native
blocking dialog with the message, **Copy details** and **Open logs**, before the process
exits non-zero. The dialog is a native message dialog, not an egui window, because no
event loop exists yet.

### User interface

A **Diagnostics** window is opened from Help → Diagnostics… (including the native macOS
Help menu) and from the startup banner.
It has these areas:

- **This session**: the current session summary, with **Copy diagnostics**,
  **Save bundle…** and **Open GitHub issue…**.
- **Incidents**: recent incidents with time, kind and message; selecting one shows its
  event, environment and log excerpt, with **Copy**, **Save** and **Open GitHub issue**.
- **Logs**: the session log level, a note when `RUST_LOG` controls it, and **Open logs
  folder**.
- **Privacy**: the **Include SQL in logs and reports** checkbox.

Every sharing action goes through a review window that shows the exact JSON and offers
**Copy reviewed bundle**, **Save reviewed bundle…** and **Open GitHub issue with this
report**. The issue page is `github.com/mceck/sqlr/issues/new`, with a title and a summary
taken from the bundle. GitHub limits the request line to a few kilobytes, so a bundle that
fits is embedded in the body inside a collapsible code block; otherwise sqlr copies the
full bundle to the clipboard and the body carries a marker for one paste. The browser
opens only after the review and nothing is submitted until the person presses **Create**
on GitHub; sqlr itself makes no request.

The startup banner is non-blocking and stays until dismissed. **Dismiss** acknowledges the
incident so it is not announced again. When the previous session ended without a panic,
the banner wording says only that the session did not close cleanly.

The result error panel adds **Copy details**, which copies the error message and hint, the
connection and database, the statement subject to the SQL setting, and the elapsed time.
It does not create an incident.

**Report a Problem…** is available from Help and from the Diagnostics window. It offers a
free-text note, checkboxes for SQL and open tab titles, **Create report and copy**,
**Create report and save…** and **Create report for GitHub…**, and records a `manual`
incident with the note. The GitHub path also goes through the review window first.

### Settings

`diagnostics.include_sql` is a global-only boolean setting, defined with the other
settings, default true, persisted in the settings file, and editable both in the
Diagnostics window and in the General preferences section. It is included in the settings
snapshot of a bundle. No other diagnostics setting is persisted; the log level, the
include-titles choice for a manual report and the retention limits are session constants
or per-report choices.

### Engine and sidecar instrumentation

The engine attaches operation context at the command boundary and propagates it through
the existing operation registry and query/session identifiers. Failure logging happens
where the outcome is final, not in every helper that sees an error. The sidecar client
logs process exit, handshake failure, protocol violations and kill reasons with the driver
identity; the forwarded driver stderr keeps its own lines.

MCP tool calls log the tool name, connection when present, duration and outcome, but never
arguments or returned data.

### Environment collection

Environment information is collected once per session and once per bundle on a background
thread, never on the interface thread. Values that cannot be obtained on a platform are
omitted rather than replaced with guesses. Resident memory is best-effort and omitted when
unavailable. The short commit hash is embedded at build time when available and omitted
otherwise.

### Documentation and conventions

The design document's error-handling section and security section are updated: the "Copy
diagnostics" action becomes real, the local-only and no-telemetry statement stays, and the
absence of symbols and backtraces in release is explicit. The usage documentation gains a
troubleshooting page covering where reports live, what they contain, how to copy, save or
report them, retention, log level and the SQL setting. The paths table in the README and in the
settings documentation gains the diagnostics directory and the fourteen-file log
retention. The project instructions gain the hard rule that `error!` means incident.

### Implementation order

1. Incident model, file store, retention, permissions and session marker/lock, with unit
   tests.
2. Log ring, logging initializer changes, retention, reloadable level, session identifier
   and the error-to-incident layer.
3. Panic hook and startup-failure handling with the native dialog.
4. Engine command panic conversion and operation failure, then failure logging and spans.
5. Sidecar crash logging.
6. Bundle composition and the SQL setting, with the anti-leak test.
7. Diagnostics window, startup banner, result Copy details and Report a Problem.
8. Documentation updates and final acceptance run.

## Testing Decisions

A good test asserts external behaviour through a public seam: files that appear, JSON that
round-trips, operation states that change, text shown in the interface. Tests do not reach
into private functions, do not depend on log formatting details beyond the fields the
contract promises, and do not require a real database server.

### Seams

The highest available seams are used:

1. **The diagnostics value and its public operations** (new): recording an incident,
   listing and reading incidents, acknowledging, pruning, building a bundle, and the
   session marker/lock lifecycle. Tests use a temporary data directory and a test session.
   This is the single new seam; everything the feature promises is observable through it.
2. **The engine's public query and operation API with the fake driver** (existing): a
   panicking fake driver must make the query and its operation end `Failed` while the
   engine continues. Existing engine and sidecar tests already use this seam.
3. **The application through `egui_kittest`** (existing): the startup banner and the
   Diagnostics window are exercised through the app harness, as the other interface tests
   are.
4. **The panic hook through a child thread** (new, but the highest possible for a global
   hook): install the hook, panic inside `catch_unwind` on a named thread, restore the
   previous hook, and assert the written file. `cargo nextest` runs each test in its own
   process, so the global hook cannot leak between tests.
5. **The filesystem in a temporary directory** (existing): permissions, retention and
   atomic rewriting are asserted on real files.

The diagnostics seam is available to tests without starting the interface; the UI seam is
used only for presentation behaviour. No test starts a real sidecar process for
diagnostics: the sidecar crash contract is asserted at the engine/fake-driver seam, and
the existing sidecar tests keep covering process death.

### Test list

- Incident serialization round-trips, including unknown `fields` and a manual note.
- Retention removes the twenty-first incident and incidents older than thirty days, and
  ignores unrelated files.
- Acknowledging survives a reload and stops the startup announcement.
- The session marker distinguishes clean exit, unclean exit and a live second instance.
- Log ring bounds by lines and by bytes, keeps the newest entries and marks truncation.
- SQL omission with `diagnostics.include_sql = false` in failure lines, incidents and
  bundles; SQL presence and 4 KiB truncation with the setting on.
- Panic hook writes message, location, named thread, session identifier and log excerpt,
  then chains to the previous hook.
- An engine panic makes the query and operation `Failed` and records an `engine_panic`
  incident; a second query still runs.
- A startup failure writes a `startup` incident (the native dialog is not asserted).
- Secrets sentinel test: a connection with sentinel password and token values produces a
  bundle in which neither sentinel occurs anywhere.
- Log and diagnostics directories and files have owner-only permissions.
- Bundle caps: many tabs, operations and log lines produce a bounded document with
  truncation markers.
- Startup banner: a previous panic shows the full banner; an unclean shutdown shows the
  lighter wording; Dismiss removes it after a restart.
- Diagnostics window: opens from Help, lists an incident, shows its log excerpt, and
  toggles the SQL setting.
- Result error panel Copy details copies the expected text.

Prior art: engine tests with the fake driver, application interface tests with
`egui_kittest`, store tests using temporary directories, and the existing logging test
that checks the stderr fallback.

### Verification commands

The feature is complete only when formatting, linting and the workspace tests pass:

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

## Acceptance Criteria

1. A failed query writes exactly one log line with operation, connection, driver,
   database, elapsed time, error kind and, when enabled, the statement.
2. A syntax, permission, authentication, timeout or cancelled operation does not create an
   incident.
3. An unexpected error at error level creates an incident file in the diagnostics
   directory.
4. A panic on the interface thread writes a crash report containing message, file, line,
   thread, session identifier and recent log lines, and the next launch shows the banner.
5. A panic in an engine command ends its operation as `Failed` with an internal-panic
   message, writes an `engine_panic` incident, and leaves other operations working.
6. A sidecar driver exit records the driver identity and reason, fails only its
   connections, and does not stop the application.
7. A failure after the data directory is known writes a `startup` incident and shows a
   native dialog before exiting non-zero.
8. A session that is killed or loses power produces the lighter unclean-shutdown notice
   on the next launch; a clean exit produces none.
9. Dismissing a startup notice prevents it from appearing again for that incident.
10. The Diagnostics window opens from Help, shows the session summary and recent
    incidents, and can copy and save a bundle.
11. A saved bundle contains the documented sections, stays within 512 KiB, and marks every
    truncated section.
12. With `diagnostics.include_sql = false`, no SQL appears in logs, incidents or bundles;
    with it true, failed SQL appears and is truncated at 4 KiB with a marker.
13. No password, passphrase, bearer token or vault value appears in logs, incidents or
    bundles, verified by sentinel values.
14. A manual report records its note, appears in the incident list, and honours the SQL and
    tab-title choices.
15. The log level can be changed for the session from the Diagnostics window, resets on
    the next launch, and `RUST_LOG` takes precedence when set.
16. Log files rotate daily, at most fourteen are kept, and incidents are pruned to twenty
    files or thirty days.
17. The logs and diagnostics directories are owner-only, and incident files are
    owner-only.
18. If the log directory is unwritable, the application starts and shows a warning that
    diagnostics are limited to stderr.
19. No diagnostics code performs a network request.
20. `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`
    and `cargo nextest run --workspace` pass.

## Out of Scope

- Remote reporting, crash upload, telemetry, an installation identifier or any network
  endpoint.
- Backtraces, symbolication, dSYM or other debug artifacts, build-identifier capture and
  address-to-symbol tooling. Release builds remain stripped and crash reports contain no
  stack trace.
- Minidumps, native signal handlers or operating-system crash-reporter integration.
  Segmentation faults and out-of-memory kills are detected only as an unclean shutdown.
- Automatic SQL literal redaction or masking. SQL is recorded as typed when enabled.
- Size-based log rotation, compression or archiving.
- Per-profile or per-connection diagnostics settings; the SQL setting is global.
- Exposing diagnostics, incidents or logs through MCP or the agent API.
- Changing what the history stores or retroactively removing SQL from existing history.
- Changing the vault, keychain or secret-storage behaviour.
- Recovering from an interface-thread panic and continuing to run.
- Retroactive capture of failures that happened before this feature existed.
- Application update checks or any unrelated networking.

## Further Notes

### Why this shape

The feature deliberately reuses the logging pipeline instead of introducing a second
observability system. `tracing` is already a workspace dependency and is already threaded
through the engine and the sidecar client; making `error!` the incident signal keeps the
producer side unchanged and gives one ingestion path. The alternative — a new diagnostics
crate with a typed sink — would be more explicit but would add a crate and dependencies to
the engine merely to write a JSON file.

Keeping the interface thread free of a frame-level `catch_unwind` is a deliberate safety
choice. A panic already means an invariant was violated; continuing risks writing over
data. The next-launch report is the honest recovery.

The absence of backtraces is a consequence of the fifty-megabyte installed-size budget and
the stripped release binary. It is stated in the user-facing documentation rather than
hidden, and the incident schema has room for a backtrace field if symbols ever become
available.

### Known limitations

- A crash report cannot contain a live snapshot of connections, tabs or operations taken
  at the moment of the crash, because the panic hook must not take application locks.
  Those sections exist in a bundle produced from the Diagnostics window and are labelled
  as the current session.
- A sidecar driver crash is reported with its exit reason and forwarded output, not with a
  stack trace from inside the driver.
- When the local SQLite database or the settings file is corrupt, the application may
  start with defaults; diagnostics are unaffected because they do not use those files.
- The environment summary is best-effort per platform: fields that cannot be obtained are
  omitted.

### Risks and mitigations

- **The `error!` convention can be misused.** A test asserts that an expected syntax error
  does not create an incident, the rule is documented as a hard rule, and code review
  should treat a new `error!` as a new incident.
- **Log volume grows.** Failure lines are one per operation, cancellation is debug-only,
  rotation keeps fourteen files, and the SQL flag can remove the largest field.
- **The ring buffer costs memory and formatting time.** It is bounded by lines and bytes
  and is written by the same layers that already format stderr and file output.
- **Sensitive content can reach a report.** Secrets are structurally excluded, paths are
  shortened, SQL can be disabled, and the bundle is previewed before sharing. Automatic
  SQL redaction is explicitly out of scope, as decided.
- **A panic inside the panic hook.** The hook uses plain file operations, does not lock
  application state, and falls back to stderr; chaining to the previous hook preserves the
  standard output.

### Documentation deliverables

- Design document: error handling and security sections updated for incidents, retention,
  startup reporting and the local-only promise.
- Usage documentation: a troubleshooting page with the diagnostics directory, incident and
  bundle contents, retention, log level, `RUST_LOG`, the SQL setting and how to share a
  report.
- Paths table in the README and settings documentation: diagnostics directory and log
  retention.
- Project instructions: the `error!`-means-incident rule and the local-only diagnostics
  rule.
