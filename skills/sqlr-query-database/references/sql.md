# SQL dialects in sqlr

`run_query` accepts one statement. sqlr splits the text with the Connection's dialect, so a trailing `;` is fine, but a second statement is refused.

## Bounding rows

| Driver | Row limit |
| --- | --- |
| PostgreSQL, MySQL, MariaDB, SQLite, DuckDB, Snowflake, SAP HANA | `LIMIT n` |
| SQL Server | `SELECT TOP (n) …` or `OFFSET 0 ROWS FETCH NEXT n ROWS ONLY` after `ORDER BY` |
| Oracle | `FETCH FIRST n ROWS ONLY` |

## Catalog questions

Prefer the tools over catalog SQL: `search_objects`, `describe_object` (columns, keys, indexes, foreign keys) and `get_ddl` work the same on every relational driver. Use `information_schema` or the system catalogs only for what the tools do not cover, such as row counts, sizes or privileges.

## Transactions per driver

- PostgreSQL: an error inside a transaction aborts it; only `ROLLBACK` (or a rollback to a savepoint) continues.
- MySQL and MariaDB: DDL commits the open transaction first.
- SQLite: one writer at a time; a long write transaction blocks other writers.
- Snowflake runs in auto-commit whatever the profile says.

## Explain

`explain_query` builds the driver's own command: PostgreSQL `EXPLAIN (FORMAT JSON)` or `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)`; MySQL and MariaDB a tree or JSON plan (analyze only for reads); SQLite `EXPLAIN QUERY PLAN` (no analyze); Oracle its text plan; SQL Server its showplan. Statements with numbered parameters (`$1`) cannot be explained on PostgreSQL: inline the values.
