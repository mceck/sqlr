# Snowflake

The built-in Snowflake driver uses the [public SQL API](https://docs.snowflake.com/en/developer-guide/sql-api) directly over HTTPS with rustls. It needs no Java, JDBC jar or ODBC installation.

## Connection

Choose Snowflake in **New connection**. Enter the account hostname, for example `myorg-myaccount.snowflakecomputing.com`, and optionally database, schema, warehouse and role. Context names are case-sensitive: use the names returned by Snowflake, usually uppercase for unquoted identifiers. The server must have a usable warehouse for queries that need compute.

Choose the authentication token type and enter its value in the secret token field:

- OAuth access token.
- Programmatic access token (PAT), with account network and authentication policies configured to allow it.
- Key-pair JWT generated externally according to Snowflake's authentication instructions. sqlr does not generate or refresh these short-lived JWTs.

Tokens go through sqlr's encrypted credential vault, never `connections.toml` or connection URLs. Paste URLs such as `snowflake://myorg-myaccount.snowflakecomputing.com/MY_DATABASE?schema=PUBLIC&warehouse=COMPUTE_WH`; enter the token separately. Certificate verification is always enabled.

## Queries and metadata

The driver supports SQL queries and DDL/DML, bound values, asynchronous server execution, cancellation and partitioned results fetched as needed. The explorer lists schemas, tables, views and columns in the selected database, and the editor loads autocomplete catalogs. Exact decimals and integers outside sqlr's signed 64-bit range are displayed as text to preserve their value.

## Session limits

Each SQL API request is independent. The driver does not advertise transactions or editable result grids. Imports and transfers into Snowflake are unavailable because sqlr requires transactional loading; exporting query results is supported. Standalone transaction and session control commands are rejected because their state would not persist to the next request. Set database, schema, warehouse and role in the connection form.

Temporary tables, session variables and context changes cannot be relied on across requests. sqlr executes scripts as separate statements; a script containing `BEGIN`/`COMMIT` does not create one SQL API transaction. Existing `jdbc-snowflake` profiles keep using their installed JDBC driver and can still be used for session-based workflows.

Snowflake determines result partition sizes. sqlr fetches one partition at a time and returns chunks of the requested number of rows. Expired tokens require reconnecting with a fresh token.

Statement errors show Snowflake's own message (with the token removed should the server ever echo it). Authentication, permission and service failures show a fixed message.

## Testing

The crate tests use a local HTTP fixture for request, polling, binding, pagination and cancellation behavior; they need no Snowflake account. A smoke test against a real account runs only when `SQLR_TEST_SNOWFLAKE_HOST` and `SQLR_TEST_SNOWFLAKE_TOKEN` are set (optionally `SQLR_TEST_SNOWFLAKE_DATABASE`, `_SCHEMA`, `_WAREHOUSE`, `_ROLE` and `_AUTH_TYPE`):

```sh
cargo nextest run -p sqlr-driver-snowflake live_smoke
```
