[Manual](README.md) › Help

# Diagnose problems and share a report

Start with the error shown beside your query or connection. Use Diagnostics for unexpected failures, local logs and a report you can review before sharing.

## Recover from common problems

| Problem | Cause to check | What to do |
|---|---|---|
| Connection fails or times out | Address, credentials, SSH, TLS or server availability | Edit the connection and use **Test**. Check the server and tunnel settings, then use **Retry** |
| Connection drops during work | Network interruption or server closed the session | Use **Reconnect** in the tab banner. Review transaction state before repeating a write |
| Query fails with a position or hint | SQL syntax, permissions or database context | Read the error and hint, correct the statement, then rerun it. Use **Copy details** to preserve the error |
| Results cannot be edited | No usable row key, unsupported result shape or driver capability | Open a base table with a primary key or non-null unique key; see [result editing](results-and-editing.md) |
| Fetching stops before all rows arrive | Row cap or memory budget reached | Close unused results or adjust the **Result grid** limits; loaded rows remain available |
| Edits conflict with another user's changes | The original row changed after you loaded it | Review the reported conflict, refresh the data and reapply the intended change |
| Dropping a database fails because it is in use | Other sessions are connected to it | Use the **Database in use** dialog to terminate the blocking sessions, then **Retry drop** |
| A feature or action is absent | The driver does not declare that capability | Check [driver support](drivers.md) and the relevant database's limitations |
| Saved credentials cannot be read | Locked keychain, denied access or damaged vault | Unlock or authorize the OS keychain and retry. Keep the existing vault; moving it alone does not transfer its key |
| MCP client cannot connect | Server disabled, occupied port or stale token | Follow the [MCP troubleshooting table](agents.md#fix-a-connection-problem) |
| Preferences cannot save | Configuration folder is unwritable | Check folder permissions or select a writable configuration directory at startup |

A syntax error, authentication error, permission denial, timeout or cancellation does not normally create an incident. It is still shown where the operation failed.

## Copy a query error

Choose **Copy details** in the result error panel. The copied text includes the message, hint, connection, database, duration and source position when available. It includes the failed statement only when SQL diagnostics are enabled. This action does not create an incident.

## Inspect local diagnostics

1. Open **Help → Diagnostics…**, or find Diagnostics in the command palette.
2. Select an incident to read its message and context.
3. Use **Open logs folder** when you need the raw log files.
4. Adjust the session's log level if you need more detail, then reproduce the issue.

The log level returns to `info` on the next launch. When `RUST_LOG` is set, it controls logging and the selector is disabled.

## Control SQL privacy

**Include SQL in logs and reports** appears in both Diagnostics and **Preferences → General**. It is enabled by default and applies globally.

> **Warning:** SQL is recorded as typed, without automatic masking of literal values. Turn this off before reproducing an issue with sensitive statements.

Disabling it omits SQL from subsequent diagnostic output. Existing files are not rewritten. A manual report can exclude SQL even when the global setting allows it. Query History is a separate local feature.

Stored passwords, SSH passphrases, bearer tokens and vault contents are excluded from diagnostics. Reports may still contain connection names, hosts, usernames, tab titles and your notes, so review the complete report before sharing.

## Prepare a report

1. Choose **Report a Problem…** from Help, the palette or Diagnostics.
2. Describe what you were doing and what you expected to happen.
3. Choose whether to **Include SQL statements** and **Include open tab titles**. SQL cannot be enabled here when the global privacy setting is off.
4. Choose **Copy report**, **Save report…** or **Prepare GitHub issue…**.
5. Read the exact JSON in the review window before choosing a final action.

Creating the report saves a manual incident locally so you can reopen it later. You can also prepare a bundle for an existing incident from Diagnostics.

The review window offers **Copy reviewed bundle**, **Save reviewed bundle…** and **Open GitHub issue with this report**. The last action opens a prefilled public issue in your browser. You submit it yourself on GitHub. If the bundle is too large for the browser URL, sqlr copies it and tells you where to paste it in the issue.

Diagnostics makes no uploads or telemetry requests. Reports stay local until you share them.

## Understand a startup notice

After a recorded panic, the next launch offers **Open report**, **Report on GitHub**, **Copy** and **Dismiss**. Dismissing acknowledges the notice; the report remains in Diagnostics. **Copy** on this banner copies the original incident JSON.

After a power loss, killed process or other abrupt termination, sqlr may only know that the previous run did not close cleanly. That notice does not identify a cause. Another running instance does not count as a crash.

A startup failure after sqlr knows its data directory creates a report and opens a native dialog with **Copy details** and **Open logs**. Invalid startup arguments are reported in the terminal instead. If the logs directory is unwritable, sqlr continues with a persistent warning and writes diagnostics to stderr.

## Find reports and understand their limits

The data directory is shown in Diagnostics. Its default locations are described in [Settings and shortcuts](settings-and-shortcuts.md#locate-your-files).

| Content | Path within data directory | Retention or limit |
|---|---|---|
| Daily logs | `logs/sqlr.log.<date>` | Newest 14 files |
| Incidents | `diagnostics/<incident-id>.json` | Newest 20 files, no older than 30 days |
| App session tracking | `diagnostics/session.json` and `diagnostics/session.lock` | Used to recognize unclean exits and running instances |
| Reviewed bundle | Your chosen save destination | Up to 512 KiB; omitted sections are identified |

A bundle includes environment information, driver versions, non-secret settings, connection metadata, operations and recent logs. SQL and individual strings are capped at 4 KiB. Paths inside your home and data directories are shortened. Release reports have no native minidump, symbols or backtrace, so include reproducible steps when reporting a failure.

## Related

- [Connections](connections.md): check addresses, TLS, SSH and credentials.
- [Results and editing](results-and-editing.md): understand editing requirements and fetch limits.
- [Agents and terminal](agents.md): resolve MCP errors and token changes.

---

← [Glossary](glossary.md) · [Manual index](README.md) →
