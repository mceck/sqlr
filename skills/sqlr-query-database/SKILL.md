---
name: sqlr-query-database
description: Explore and query the user's databases on your own through the sqlr MCP tools - find tables, read columns, keys, indexes and DDL, run SQL or native commands (MongoDB, Elasticsearch, Redis), page results, modify data under the user's safety rules, and read Explain plans. Use when the user asks a question about their data, wants to find a table or collection, inspect a schema, run or change something in a database exposed in sqlr, or understand why a query is slow.
metadata:
  sqlr-version: 0.1.4
---

# Query databases through sqlr

sqlr gives you your own Session on each Connection the user exposed: your queries never share a transaction with the user's tabs or with other agents. Everything you run shows in sqlr's Operations panel and History.

If the sqlr tools are missing or failing, use the `sqlr-connect-agent` skill. To work in the query the user has open, use `sqlr-edit-user-query`.

## 1. Find the Connection

`config_list` with `connections` lists the exposed Connections: id, name, driver, folder, profile and Agent access. Always use the id, never the name, in later calls.

- `Read` access: reads only; every modifying statement answers `read_only`.
- `Write` access: modifications are allowed, under the profile's rules (see below).

If the Connection the user means is not listed, it is not exposed: ask the user to expose it.

## 2. Find the objects

- `search_objects` finds tables and views by name across the exposed relational Connections. Start here when you only know a name or part of one.
- `describe_object` gives a relational table's columns (type, nullability, default, generated), primary key, unique keys, indexes and foreign keys. Use the foreign keys to understand joins.
- `browse` walks the Explorer tree of any family: call it with an empty path for the top level, then pass a returned `path` unchanged to go one level down. It is the way into MongoDB collections, Elasticsearch indices and Redis keys.
- `get_ddl` returns the `CREATE` script of a Node (table, view, function, index…) as sqlr's DDL viewer writes it; pass a `browse` path.

Catalog answers may come from sqlr's cache (`from_cache`); that is normal.

## 3. Run a query

`run_query` runs exactly **one** unit: one SQL statement, one MongoDB shell statement, one Elasticsearch console request or one Redis command. A SQL script with several statements answers `invalid`; run the statements one by one, or put the script in a tab (`sqlr-edit-user-query`).

Write queries that keep results small:

- always bound the rows (`LIMIT`, `TOP`, `FETCH FIRST`, `.limit()`, `size`);
- select the columns you need, not `SELECT *`, on tables you have not described;
- aggregate in the database (`count`, `group by`) rather than fetching rows to count them.

The answer is a tagged Result:

- `kind`: `tabular`, `document` or `reply`, and only that member is present;
- `status`: `ready`, `running`, `cancelled` or `failed`; a failed statement still returns a successful tool call with `error` filled in;
- `truncated`: more rows exist; page them with `get_query_result` using `offset` and `limit`;
- `query_id`: keep it to page, to `cancel_query`, and to `close_result` when you are done.

Values: SQL `NULL` is JSON `null`; decimals, dates, UUIDs and similar are strings exactly as sqlr displays them; binary is base64.

Limits: a call waits 30 s by default (`timeout_s` 1–120); a page holds at most 1000 rows and 256 KiB (200 rows by default). A query still running at the deadline answers `status: running` with a `query_id`: poll `get_query_result`, or `cancel_query` it. A Result is kept for 5 minutes after its last use; close it with `close_result` as soon as you are done.

## 4. Modify data

On a `Write` Connection, `INSERT`, `UPDATE`, `DELETE`, DDL and native writes go through `run_query`. The Connection profile decides what happens:

- read-only profile → `read_only`;
- confirmation for writes or destructive statements → the call waits while the user decides in sqlr's Approval dialog, up to 60 s; `denied` and `timeout` are the user's answer, so do not resubmit the same statement;
- otherwise the statement runs at once.

Before a modification, say what it will change; for an `UPDATE` or `DELETE`, count the matching rows first with the same `WHERE`.

### Transactions

Your Session follows the profile's commit mode (`config_list` with `profiles`):

- `auto`: every statement commits on its own;
- `smart`: reads commit on their own; the first write opens a transaction that stays open;
- `manual`: every statement runs in a transaction that stays open.

In `smart` and `manual`, finish with an explicit `COMMIT` (or `ROLLBACK`) as its own `run_query` call, after telling the user what you changed. Never leave a transaction open at the end of your work: when the MCP session ends, sqlr rolls it back and the work is lost. MongoDB, Redis and Elasticsearch have no transactions; their writes apply immediately.

## 5. Explain a query

`explain_query` returns the plan of one statement: planning and execution time, cost, estimated and actual rows, plain-language warnings, the operator mix and the raw plan. Without `analyze` the statement does not run. With `analyze` it runs: on a `Read` Connection only for reads; for a write it needs the user's Approval and runs in a transaction that is always rolled back. To show the plan to the user instead, use `show_in_app` with the `explain` view.

## Per family

Read the reference for the Connection's family before writing native commands:

- [SQL dialects](references/sql.md)
- [MongoDB](references/mongodb.md)
- [Elasticsearch](references/elasticsearch.md)
- [Redis](references/redis.md)

## Rules

- Never ask for or print credentials; Connections are write-only and secrets stay in sqlr.
- Do not retry a statement after `denied`, `timeout` or `read_only`.
- Report errors with their code and message; free-form server messages are already sanitized.
