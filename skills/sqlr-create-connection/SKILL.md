---
name: sqlr-create-connection
description: Create a new database Connection in sqlr through the sqlr MCP tools and make sure it works. Use when the user asks to add, set up or register a database, server or file in sqlr (PostgreSQL, MySQL, MariaDB, SQLite, SQL Server, Oracle, MongoDB, Redis, Elasticsearch, DuckDB and others), gives a connection URL, or when a Connection you created answers secret_required or fails its test.
metadata:
  sqlr-version: 0.1.3
---

# Create a Connection in sqlr

A Connection is a saved set of settings for one database server or file. sqlr keeps its values write-only for agents: you can set hosts, users and passwords, but you never read them back. Secrets live in sqlr's encrypted vault.

If the sqlr tools are missing or failing, use the `sqlr-connect-agent` skill.

## Steps

1. **Pick the driver.** Call `config_list` with `drivers` (optionally `driver_id`). Each manifest lists the fields: key, label, type, default, whether it is required, whether it is secret, and its group. Use the field keys exactly as listed.
2. **Ask for the values.** Ask the user for what the manifest needs: host, port, database, user, and the secret fields. Never guess a host, user, database or password, and never invent one "to try". If the user pastes a connection URL (`postgresql://user@host:5432/db`), split it into the fields; a password inside the URL is a secret field.
3. **Choose where it goes.** Ask, or infer from the request, the folder (`config_list` with `folders`) and the profile. A production database goes on the Production profile, which asks before writes and uses manual commit; staging usually goes on Test; a local database on Development.
4. **Create it.** `config_write` with target `connection`, the driver, a clear name, the folder, the profile and the non-secret values. The answer gives the new Connection id. Keep it: the Connection starts with Agent access `None`, so it will not appear in `config_list` connections, but this MCP session can still finish and test it.
5. **Write the secrets.** `config_write` on the same id with the secret fields. Agree `save_secrets` with the user: on, the secrets go to the vault and survive restarts; off, they stay in memory until sqlr closes and the user types them again next time.
6. **Test it.** `test_connection` with the id. On success it reports the server version. When secret fields are empty the call fails with `secret_required` naming them: ask for those values. Otherwise a failed test reports one of:
   - `auth`: wrong user or password, or the user lacks access to that database. Ask the user to check them, then rewrite the secrets.
   - `connection`: wrong host or port, the server is down, or a firewall or SSH tunnel is needed. Confirm the address with the user.
   - `tls`: the server requires TLS or the certificate does not verify. Check the TLS fields in the manifest with the user. Do not turn certificate verification off without the user asking.
   - `timeout`: the server did not answer in time; usually a network path or a tunnel.

   Patch only the fields that change (`config_write` is partial; `remove_values` and `remove_secrets` clear fields) and test again.
7. **Hand it back.** Tell the user the Connection works, where it is (folder and profile), and that Agent access is `None`. If they want agents to query it, they set Agent access to `Read` or `Write` in sqlr themselves; agents cannot change it.

## Fixing a Connection

A Connection created in this MCP session, or one the user exposed, can be patched with `config_write`. If it was created in an earlier session and is not exposed, it is the user's: ask them to fix it in sqlr or to expose it.

## Rules

- Secrets go only into `config_write` secret fields: never into the name, a regular field, a log, a file or a chat summary.
- `agent_access` and the driver of an existing Connection cannot be written.
- Moving a Connection to a looser profile is refused; see the `sqlr-configure` skill.
- Deleting a Connection uses `config_delete` and always waits for the user's Approval.
