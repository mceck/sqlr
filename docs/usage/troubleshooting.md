# Troubleshooting

Open **Help → Diagnostics…** to see the current session, installed drivers and recent incidents. Diagnostics work independently of the local history database and credential vault. Reports stay on your computer: there are no uploads, network requests or telemetry. Sharing is always an explicit action.

## Copy, save or report a problem

Choose **Copy diagnostics** or **Save bundle…**, review the JSON preview, then use **Copy reviewed bundle** or **Save reviewed bundle…**. The bundle describes the current session: application version and build, system, drivers, sanitized settings, connection metadata, open tabs and transactions, recent operations, logs and the selected incident. It does not claim that current tabs or connections describe the moment of an earlier crash.

## Open a GitHub issue

From the same preview, choose **Open GitHub issue with this report**. sqlr opens a new public issue in your browser with a title and a summary already filled in. When the whole bundle fits in the prefilled URL it goes into the issue body; otherwise sqlr copies the bundle to the clipboard and the issue asks you to paste it at the marker. Nothing is submitted until you press **Create** on GitHub, and sqlr never makes the request itself.

Use **Help → Report a Problem…** to add a note and choose whether to include SQL and open tab titles. Creating the report saves a manual incident that can be reopened later. A result error's **Copy details** copies its message, hint, connection, database, duration and permitted SQL without creating an incident.

Bundles are valid JSON capped at 512 KiB, with explicit truncation markers. Paths under your home and data directories are shortened to `<home>` and `<data-dir>`. Passwords, SSH passphrases, bearer tokens and vault contents are excluded. Review other potentially sensitive content, including connection names, hosts, usernames, notes and SQL, before sharing.

## SQL privacy

**Include SQL in logs and reports** is available in Diagnostics and General preferences. It defaults to enabled and is persisted globally as `diagnostics.include_sql`. SQL is recorded as typed, without automatic literal masking, up to 4 KiB with a truncation marker. Disable the setting to omit SQL from subsequent logs and reports; manual reports can also exclude SQL individually. Existing files on disk are not rewritten.

## Crashes and startup failures

A panic writes an incident before the process exits. On the next launch, a non-blocking banner offers **Open report**, **Report on GitHub**, **Copy** and **Dismiss**. Copy on the banner copies the original incident JSON. Dismiss is remembered, and the report remains in Diagnostics.

A killed process, segmentation fault, power loss or out-of-memory termination may leave no panic report. The next launch only says that the previous session did not close cleanly. A clean exit produces no notice; another running instance does not count as a crash.

An engine command panic fails its operation and leaves unrelated work running. A driver process crash fails only its own connections. Release binaries are stripped: reports contain the panic message and source location when available, but no backtrace, symbols or native minidump.

A startup failure after the data directory is known creates a startup incident and opens a native error dialog with **Copy details** and **Open logs**. Invalid command-line arguments remain terminal errors.

## Files, retention and log level

Files live under the data directory shown in Diagnostics:

| Content | Path | Retention |
|---|---|---|
| Daily logs | `logs/sqlr.log.<date>` | Newest fourteen files |
| Incidents | `diagnostics/<incident-id>.json` | Newest twenty, no older than thirty days |
| Session marker and lock | `diagnostics/session.json`, `diagnostics/session.lock` | Used to distinguish a crash from a running instance |

Use **Open logs folder** to inspect raw logs. Logs and diagnostics directories and incident files are owner-only. Reports require no SQLite database, vault or working settings file. If the log directory is unwritable, sqlr still starts and shows a persistent warning that diagnostics are limited to stderr.

Choose error, warn, info or debug in Diagnostics to change the running session's log level. It resets to info on the next launch. If `RUST_LOG` is set, the environment controls the level and the selector is disabled. Cancellation is logged only at debug level; ordinary syntax, permission, authentication and timeout errors do not create incidents.
