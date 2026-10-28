[Manual](README.md) › Administration

# Manage database access and server activity

Open administration tabs from a connected database's explorer menu. The driver decides which panels appear; your database account still needs the server permissions required by each action.

## Manage users and roles

1. Open **Users and roles** from the connection's administration actions.
2. Select an existing role, or choose **New role**.
3. Review its name, attributes, membership and database-specific fields. MySQL accounts also have a host.
4. Select the relevant schema and object kind under **Privileges**, then change the grant checkboxes.
5. Choose **Preview** to inspect the SQL.
6. Choose **Apply** or press ⌘S to write the changes, or **Revert** to discard them.

The panel keeps edits pending until Apply. A new password is hidden in SQL previews and diagnostic output. Password fields do not reveal a role's current password.

For MySQL, the privilege grid does not list global grants on `*.*`. Manage those with explicit `GRANT` or `REVOKE` SQL in an editor.

To remove a role, select it and choose **Drop role…**. Check the confirmation carefully, particularly when removing the account you are using. The database may reject removal while objects or memberships depend on it.

Administrative writes require a writable session. If a transaction is open on the connection used by the panel, commit or roll it back before applying administrative changes.

## Inspect sessions and blocking locks

Open **Sessions and locks** to see server-side sessions, including their user, database, client, state, wait reason, running time and current query when the server provides them.

1. Use **Refresh** (F5 or ⌘R) for a current snapshot, or leave automatic updates running while the panel is visible.
2. Enable **Hide idle** to focus on active work.
3. Select a session to inspect its details and locks.
4. Follow the **Blocking** tree to identify the session other work is waiting for.

| Action | Result |
|---|---|
| **Cancel query** | Asks the server to stop the current statement and keep the session connected |
| **Terminate** | Closes the server session, interrupting its work |

Confirmations follow your connection profile, with extra care for your own session. Terminating the session sqlr is using can disconnect the corresponding connection. A database account without sufficient privileges receives a server error rather than elevated access.

## Inspect server properties and sizes

Open **Server properties** and choose a pane:

| Pane | Contents |
|---|---|
| **Properties** | Server version and configuration values, with search |
| **Database sizes** | Storage sizes reported by the server |
| **Table statistics** | Table estimates, sizes and other supported statistics |

Use **Refresh** after making changes elsewhere. Statistics can be estimates and may depend on when the server last collected them. The schema selector limits table statistics where supported.

## Monitor sqlr operations

The Operations panel tracks client work, including connecting, catalog reads, statements, fetches and data tools. It is separate from the database's server-session list.

1. Open Operations through the activity indicator, command palette or ⌘⇧O.
2. Find the operation by connection and description.
3. Check elapsed time, state and progress.
4. Cancel the individual operation, or use **Cancel all** and confirm once.

Cancellation of a connection attempt stops the wait without waiting for its timeout. For statements, sqlr requests server cancellation when the driver supports it. For imports and restores, read the final summary because previously committed work can remain.

## Related

- [Profiles and transactions](profiles-and-transactions.md): configure confirmations and read-only access.
- [Import, export and backup](moving-data.md): understand cancellation during writes.
- [Troubleshooting](troubleshooting.md): investigate a failed operation.

---

← [Schema tools](schema-tools.md) · [Document and key-value databases](nosql.md) →
