# Import, export, transfer and backup

sqlr has four ways to move data, from the quickest to the most thorough.

| Tool | Best for |
|---|---|
| Copy from the grid | A few rows, into a chat or a document |
| Export a result | Saving what a query returned to a file |
| Transfer data | Copying between two databases, or between a database and a file |
| Dump and restore | A faithful backup using the database's own tools |

## Export a result

Run a query, then choose **Export result** from the palette (⌘⇧P) or the result's menu. Pick a format and a file name. Supported formats:

- CSV and TSV
- JSON (an array of objects) and JSON Lines (one object per line)
- SQL `INSERT` statements
- Markdown tables

Export streams straight from the database to the file, so exporting a table with millions of rows does not need to hold it all in memory, and you can cancel it midway.

By default exports write raw values. If you want them to follow your display formats for dates, numbers and so on, switch on **Use these formats for export** in Settings under Data formats.

## Transfer data

The transfer wizard copies data between any two places:

- **Sources:** a table, the result of a query, or a file (CSV, TSV, JSON, JSON Lines, Excel `.xlsx`, Parquet)
- **Targets:** an existing table, a new table, or a file in any of those formats

Because sources and targets can be on different connections, you can copy from PostgreSQL to SQLite, from MySQL to Oracle, or from a spreadsheet into a table, with the same steps.

The wizard walks you through:

1. **Source.** Pick a connection and a table or query, or pick a file. The wizard shows a sample of what is there so you can check you picked the right thing.
2. **Target.** Pick an existing table, or create a new one. For a new table sqlr writes the `CREATE TABLE` for you and maps column types between the two databases' dialects.
3. **Column mapping.** Each source column is matched to a target column, or skipped. You can convert the type and apply a transform per column: trim, upper case, lower case, empty text to NULL, or a constant value.
4. **Options.** Batch size, whether to commit after each batch or once at the end, whether to truncate the target first, whether to **upsert** by key instead of inserting, and whether to switch off constraints and indexes during the load.
5. **Review and run.** You see a summary of what will happen, then run it.

Data flows in bounded batches, so memory use stays small whatever the table size. When it finishes you get a summary of rows read, written and rejected. Rejected rows can be saved to a file so you can fix and retry them. For file imports, a bad row is reported with its line number.

### Saving a transfer as a task

Any wizard can be saved as a named **task**. A saved task keeps the settings (but never passwords) and can be run again from the Tasks panel, which also lists running jobs and past runs with their logs. Tasks run when you start them.

## Dump and restore

Dump and restore use the database's own command-line tools, which is the most faithful way to take a backup:

| Database | Tools | Formats |
|---|---|---|
| PostgreSQL | `pg_dump`, `pg_restore`, `psql` | plain SQL, custom archive, tar, directory (parallel) |
| MySQL and MariaDB | `mysqldump`, `mysql` | SQL script |
| SQLite | the SQLite backup | SQL script, or a copy of the database file |

Start from the database in the explorer and choose the dump or restore action. The dialog lets you choose what to include and how, for example compression level, schema only or data only, the number of parallel jobs for PostgreSQL's directory format, and for MySQL whether to include routines, events and triggers and whether to use a consistent snapshot.

Before it runs, sqlr shows you the exact command line. While it runs, the tool's output streams into a live log, and Cancel stops the process.

sqlr looks for the tools on your `PATH` and in the usual install places (Homebrew, Postgres.app). If it cannot find one, it tells you how to install it. You can also set an explicit path for a connection.

Passwords never appear on the command line. They are passed to the tool through its environment or a temporary file that only you can read, and removed afterwards. If the connection uses an SSH tunnel, the tool goes through the same tunnel.
