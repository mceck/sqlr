# Administration

For databases that support them, sqlr has panels for the jobs a DBA does every week. They open as tabs from the connection in the explorer. A panel only appears when the database can do what it shows.

## Users and roles

The users panel lists the roles and users of the server. Select one to see and edit its attributes, and below them a grid of **privileges** for each kind of object. Tick or untick what the role may do.

Like every other editor in sqlr, changes are pending until you act on them. **Preview** shows the `GRANT`, `REVOKE` and `ALTER ROLE` statements, **Apply grants** runs them, and **Revert** puts the panel back as it was. **Drop role** removes a role, and asks for confirmation according to your profile.

Passwords you type are used to build the statement and are masked everywhere sqlr could print them.

## Sessions and locks

The sessions panel shows what the server is doing right now: who is connected, what each session is running, how long it has been going, and what it is waiting for. When one session blocks another, the lock view shows the chain as a tree, with the session at the root that everyone else is waiting for.

Two actions apply to a session:

- **Cancel** stops the statement it is running and leaves the session connected
- **Terminate** closes the session

Both go through the confirmations of your connection's profile, so a Production connection asks first.

## Server properties and statistics

The server panel lists the server's settings and version with a search field, the size of each database, and per-table statistics such as row estimates and sizes. Use **Refresh** to reload them.

## The Operations list

Separate from the server's own sessions, sqlr keeps a list of what the app itself is doing: connection attempts, schema reads, running queries, fetches, scripts, exports and transfers. Open it from the activity indicator in the toolbar or from the palette.

Each entry shows what it is, which connection it belongs to, how long it has run and its progress. Cancel any entry, or **Cancel all** (sqlr asks once). Cancelling a connection attempt that is waiting on an unresponsive host ends it right away instead of waiting for the timeout.

Stopping a query asks the database itself to cancel it where the database supports that (for example `pg_cancel_backend` on PostgreSQL and `KILL QUERY` on MySQL), so the work stops on the server too.
