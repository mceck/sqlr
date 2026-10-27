---
status: done
---

# Connections, Credential vault and connection migration

## Specs

- [Connections](../specs/connections.md)
- [Query execution](../specs/query-execution.md)
- [Drivers](../specs/drivers.md)
- [Agents and terminal](../specs/agents-and-terminal.md)

## Problem Statement

Before a user can run a single query they have to describe where their databases are: hosts, ports, users, files, SSH bastions, certificates and passwords, often dozens of them across environments. Other clients store passwords in plain files or ask for them over and over, offer different forms for every database, and lock the user's connection list inside their own format. A pasted URL with a password ends up in logs and screenshots.

## Solution

sqlr keeps every Connection in a readable `connections.toml`, organised in Folders the user arranges by drag and drop, and every secret in one encrypted Credential vault whose key the OS keychain protects. Each Driver generates its own form from its manifest, a pasted Connection URL fills it (passwords refused), Test connection reports the server and timing, and SSH tunnels with a jump host and TLS work for every Driver. The welcome screen offers Known databases, recent Connections and a Demo database, and connections move in and out of DBeaver, DataGrip, TablePlus, pgAdmin, Beekeeper Studio and Tabularis.

## User Stories

1. As a user, I want to save a Connection with a name, Folder and color, so that I can find it again quickly.
2. As a user, I want to group Connections in nested Folders, so that I can organise them by client or environment.
3. As a user, I want to drag Connections and Folders to reorder them and move them between Folders, so that the explorer matches how I think.
4. As a user, I want the order I arranged to survive restarts, so that I do not rearrange every day.
5. As a user, I want deleting a Folder to keep its Connections, so that tidying up never loses a Connection.
6. As a user, I want to rename a Folder and have every Connection inside follow, so that renames are one step.
7. As a user, I want each database to show the fields it needs (a file path for SQLite, a host and port for servers), so that the form is never confusing.
8. As a driver author, I want my Driver's form generated from its manifest, so that I write no UI code.
9. As a user, I want to paste `postgresql://user@host:5432/db?sslmode=require` and have the fields filled, so that I can copy settings from documentation or a colleague.
10. As a user, I want a URL with a password refused and hidden, so that my password never lingers in a text box or a log.
11. As a user, I want to test a Connection and see the server version and timing, so that I know it works before saving.
12. As a user, I want to see the SSH host key fingerprint when testing through a tunnel, so that I can verify the server.
13. As a user, I want to choose whether sqlr saves my password, so that I can keep sensitive passwords out of storage.
14. As a user who does not save passwords, I want to be asked once per app run, so that I am not prompted on every query.
15. As a user, I want to duplicate a Connection with its secrets, so that I can create a similar one quickly.
16. As a user, I want deleting a Connection to delete its secrets, so that nothing is left behind.
17. As a user behind a bastion, I want SSH tunnels with password, key file or agent authentication, so that I can reach private databases.
18. As a user behind two hops, I want a jump host, so that I can reach databases behind a second bastion.
19. As a security-conscious user, I want unknown SSH host keys refused unless I opt in, and changed keys always refused, so that a spoofed server is caught.
20. As a user, I want TLS with system roots, a custom CA, client certificates and a verification choice, so that I can reach managed databases securely.
21. As a user of CockroachDB, Redshift, TiDB or Neon, I want sqlr to know they speak PostgreSQL or MySQL and preset the form, so that I connect without guessing.
22. As a user, I want the catalog to say honestly which databases are bundled, compatible, installable or planned, so that I do not try something that does not work.
23. As a new user, I want a welcome screen with database cards, recent Connections and a demo database, so that I can start in one click.
24. As a new user without a database at hand, I want a realistic demo database generated locally, so that I can try every feature safely.
25. As a user, I want the demo to leave my own `demo.sqlite` untouched, so that sqlr never overwrites my files.
26. As a user, I want my secrets encrypted at rest with a key in the OS keychain, so that a copied config folder does not leak passwords.
27. As a user, I want one keychain prompt per app run at most, so that sqlr does not nag me for every Connection.
28. As a user upgrading from an older version, I want my old keychain secrets migrated automatically and safely, so that nothing is lost or duplicated.
29. As a user running two sqlr windows, I want concurrent vault writes to be safe, so that the vault never corrupts.
30. As a user, I want a corrupt vault or a missing key to fail clearly without replacing my data, so that I can recover.
31. As a user moving from DBeaver, DataGrip, TablePlus, pgAdmin, Beekeeper Studio or Tabularis, I want sqlr to find and import my connections, so that switching is quick.
32. As a user importing, I want duplicates skipped and names kept unique, so that importing twice is harmless.
33. As a user importing an untrusted file, I want only known connection fields kept and no Agent access granted, so that the file cannot smuggle settings in.
34. As a team lead, I want to export Connections, optionally with credentials and a clear warning, so that I can share a setup.
35. As a user, I want exported files readable only by me, so that a credential export does not leak on a shared machine.

## Implementation Decisions

- Core holds the Connection model (id, name, Driver id, Folder, color, values, profile id, save secrets flag, Agent access), secrets with key-only debug output, URL parsing, redaction and URL-to-form mapping, and the Known database catalog with four statuses.
- The store's Connection repository owns `connections.toml` (layout version, Folder list, Connections in display order), Folder operations (create, rename, delete, move) and Connection moves with before, after, end and into targets, and imports in one atomic write. Writes use a uniquely named temporary file and a rename.
- The store's Credential vault implements the secret-store interface: AES-256-GCM with a random nonce per write and the header as associated data, a 256-bit key in the keychain under an account derived from the canonical config directory, an in-process lock plus an OS file lock, re-read before every change, private temporary file, sync and atomic replace, zeroised buffers, tombstones and legacy-pending marks for migration. The MCP token store shares the vault.
- The app's store worker thread runs every repository, vault and migration operation in order and answers with the current listing; the connection dialog, secrets prompt and migration dialog talk to it through operations and replies.
- The connection form widget builds fields from the manifest, keeps secrets apart from values, applies URLs, validates, and returns actions (test, save, cancel, pick file).
- The engine adds the SSH field group to every manifest with the SSH Capability, reads missing secrets from the vault on a blocking worker once per connect, opens one tunnel per Connection (jump host, host-key checking against `known_hosts`, keepalive, liveness check), and records recent Connections.
- Test connection uses a temporary `test:` Session with the Development profile and reports the connect report (time, host key).
- The Demo database is generated through the engine on a temporary SQLite Session with one transactional apply and a marker table.
- Client migration adapters are pure parsers and encoders per format; discovery and file reads run on the store worker. Imports pass through a portable-field allowlist and a duplicate identity check.

## Testing Decisions

- Store seam: repository and vault tests on temporary directories with an in-memory keychain fake: Folder operations and ordering, legacy files, concurrent atomic writes, vault encryption, missing or invalid key, corrupt vault, lazy migration with interruption and tombstones, MCP token separation. Prior art: the store's vault, store and client migration test suites.
- Core seam: unit tests for URL parsing, refusal and redaction (including half-typed passwords and IPv6) and the Known database catalog's consistency with the shipped Drivers.
- Engine seam: SSH tests against an in-process SSH server (tunnelled TCP, key file, bad authentication, unknown and changed host keys), connect timeout and cancellation with the fake Driver.
- App seam: `egui_kittest` tests of the connection dialog (drafts across data sources, failed secret reads, emptied secrets deleted, file pickers), the welcome screen and the demo database generation against a real SQLite file.
- Migration seam: fixtures per client format for import and export, credential handling and the allowlist.

## Acceptance Criteria

- [x] Connections and Folders are saved in `connections.toml` in the order the user arranged, without secrets.
- [x] Drag and drop moves Connections and Folders before, after or into targets, refusing moves into themselves.
- [x] Deleting a Folder moves its contents to its parent.
- [x] The connection form is generated from the Driver manifest, with validation of required fields, ports, numbers and choices.
- [x] A Connection URL fills the form, matching parameters to fields ignoring case, `_` and `-`.
- [x] A URL with a password is refused and only its redacted form stays in the box.
- [x] Test connection reports the server product and version.
- [ ] Test connection reports the server round-trip time (it reports the total connect time, SSH setup included).
- [x] With "Save password" off, secrets are asked on connect and kept in memory until quit.
- [x] Deleting a Connection deletes its secrets and its recent entry.
- [x] SSH tunnels support password, key file and agent authentication and one jump host.
- [x] Unknown host keys are refused unless "Trust new host keys" is on; changed keys are always refused.
- [x] TLS through a tunnel verifies the host name the user entered.
- [x] TLS supports system roots, a custom CA, client certificates and a verification mode through each Driver's TLS fields.
- [x] The Known database catalog lists bundled, compatible, installable and planned databases, and compatible ones preset the form.
- [x] The welcome screen shows driver cards, up to 8 recent Connections and Open demo database.
- [x] The Demo database is generated deterministically and never overwrites a foreign `demo.sqlite`.
- [x] Secrets are stored in `secrets.vault` with AES-256-GCM and a fresh nonce per write, under a keychain key scoped to the config directory.
- [x] The vault key is read once per App session and retried after a denial; there is no plaintext fallback.
- [x] Vault writes take an in-process and an OS file lock and replace the file atomically after syncing.
- [x] A corrupt vault or missing key fails without replacing data or creating a new key.
- [x] Old per-connection keychain items and the old MCP token migrate lazily, persisting before deleting, with tombstones.
- [x] Connection import recognises sqlr, DBeaver, DataGrip, TablePlus, pgAdmin, Beekeeper Studio and Tabularis files and discovers installed clients.
- [x] Imports skip duplicates, keep only portable fields, use fresh ids and never grant Agent access.
- [ ] Imported SQL Server connections use the bundled SQL Server Driver, and SQL Server Connections can be exported to DBeaver, DataGrip and TablePlus.
- [x] Export with credentials is opt-in each time, warns about plaintext, and writes owner-only files.
- [x] The UI thread performs no file, keychain or vault I/O for any of this.

## Out of Scope

- Master-password or plaintext secret storage.
- SSH host certificates and more than one jump host.
- Kubernetes and AWS SSM transports from imported Tabularis profiles.

## Further Notes

This task records retroactively the work that built the area as it exists today. The second unticked criterion is a code inconsistency: the client migration adapters name the SQL Server Driver `mssql`, while the bundled Driver's id is `sqlserver`.
