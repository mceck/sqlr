# Profiles, transactions and safety

Databases that matter deserve some care. sqlr gives each connection a **profile** that decides how its changes are committed and how much confirmation it asks for before it changes anything.

## Profiles

A profile has:

- a **name** and a **color**. The color tints the toolbar, the tabs and the connection in the explorer, so you always know where a query is about to run
- a **commit mode**: Auto, Manual or Smart
- **confirm writes**: ask before any statement that changes data
- **confirm destructive statements**: ask before `DROP`, `TRUNCATE`, and `DELETE` or `UPDATE` without a `WHERE`
- **read-only**: refuse statements that write
- an optional **idle transaction timeout**: after N minutes with an open transaction and no activity, sqlr asks whether to commit or roll back

Three profiles come built in, and you can edit them, reset them or add your own from **Connection profiles**:

| Profile | Color | Commit mode | Confirmations |
|---|---|---|---|
| Development | none | Auto | destructive statements only |
| Test | amber | Smart | destructive statements only |
| Production | red | Manual | every write and every destructive statement |

Pick the profile when you create or edit a connection. It is saved with the connection.

## Commit modes

**Auto.** Every statement commits by itself. If you type `BEGIN` yourself, sqlr keeps that transaction open as you asked.

**Manual.** sqlr opens a transaction before your first statement and keeps everything inside it until you press **Commit** or **Rollback**. The next statement starts a new transaction.

**Smart.** Behaves like Auto while you only read. The moment a statement changes data or schema, that statement and everything after it runs in a transaction, as in Manual. When you commit or roll back, you are back to Auto. It gives you a safety net for edits without making plain SELECTs wait on a commit.

You can change the commit mode for the current tab from the toolbar without touching the profile.

### While a transaction is open

- The toolbar shows **Commit** and **Rollback**.
- The status bar shows how many statements the transaction holds and since when.
- The connection in the explorer carries a mark that a transaction is pending.
- Result pages you scroll are read inside the transaction, so you see your own uncommitted changes.
- If a statement fails inside a transaction, the transaction stays open. PostgreSQL marks it as aborted, and the status bar tells you that only Rollback is possible.
- Closing the last tab, disconnecting or quitting asks what to do with it: Commit, Roll back or Cancel.

### Savepoints

In Manual and Smart mode you can create named savepoints inside a transaction and roll back to any of them, without losing the work before it.

## Read-only

A connection can be read-only because its profile says so, or because you flip the switch in the status bar for the current session. Where the database supports it, sqlr asks the server to enforce it (PostgreSQL's `default_transaction_read_only`, MySQL's `SET SESSION TRANSACTION READ ONLY`, a read-only open for SQLite). sqlr also checks each statement itself as a second line of defence. Results from a read-only connection are never editable.

Settings are layered: global, then profile, then connection. A lower level can only make the safety settings stricter. If the global setting is "ask before writes" a connection cannot switch that off.

## Databases without transactions

Redis, Elasticsearch and MongoDB outside sessions have no transaction to commit, so the commit mode does not apply to them. The color, the confirmations and read-only still do.
