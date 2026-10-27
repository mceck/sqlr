---
status: done
---

# Explorer, catalog, object editors and administration

## Specs

- [Explorer and objects](../specs/explorer-and-objects.md)
- [Drivers](../specs/drivers.md)
- [Connections](../specs/connections.md)
- [Query execution](../specs/query-execution.md)
- [Architecture](../specs/architecture.md)

## Problem Statement

The Explorer is where a database client is judged. Users expect to open a connection, see schemas and tables immediately, expand only what they need, and reach any object in a couple of clicks. Clients commonly fall short in three ways: opening a connection blocks on reading the whole catalog, so large schemas take seconds; a refresh after a change leaves stale objects in the tree; and the catalog is lost on restart, so every session starts with the same wait. Browsing deep into a schema often ends in a wall of names with no counts, no filter and no explanation when loading fails.

Once users find an object they need more than a name: the DDL behind it, a generated statement to query it, a way to edit its columns and indexes without leaving the client, and administration views for users, privileges, sessions and locks. Object editing is risky when a client guesses the change, applies it silently or leaves an editor open over a definition that has since changed. Administration is risky when a terminate or a revoke happens on a single click.

## Solution

The Explorer is a lazily loaded tree of Connections, schemas and objects, fed by background catalog loads with per-node states, counts, a name filter and inline errors with retry. The Catalog cache keeps what was loaded per Connection and database, survives restarts, refreshes in the background, and is invalidated when a DDL statement, a delete or a manual refresh changes what it describes.

Objects open in tabs: a DDL viewer that shows and copies the server's definition, generated SQL templates that open in an editor tab, and object editors that derive a Schema change list from the difference between the loaded definition and the user's edits, preview the exact DDL, and apply it in a transaction or statement by statement according to the driver. Create dialogs, rename, drop and truncate complete the object actions with confirmations. Administration panels show users, roles and grants, server sessions and locks with cancel and terminate, server properties, sizes and table statistics, and appear only where the driver supports them.

## User Stories

1. As a user with several connections, I want them listed with their folders, so that I can organise and reach each one quickly.
2. As a user, I want a connection to show its databases, schemas and objects as I expand it, so that I am not waiting for a full catalog read.
3. As a user, I want rows and object names to appear with counts, so that I know the size of what I am about to open.
4. As a user, I want to filter the tree by name, so that I can find a table in a large schema without scrolling.
5. As a user, I want a node that fails to load to show the error inline with retry, edit and details, so that I can fix the cause without losing my place.
6. As a user, I want a refresh on a node or the whole tree, so that changes made elsewhere appear when I ask for them.
7. As a user, I want the tree to show each connection's state and commit mode, so that I know whether I am in autocommit or a transaction.
8. As a user, I want context menus that offer only what applies to the node I right-clicked, so that I am not offered actions that would fail.
9. As a user, I want to open a table's data, its DDL, its definition editor and its generated statements from the tree, so that everything about an object starts in one place.
10. As a user, I want document collections and key-value databases to have tree shapes suited to them, so that browsing feels native rather than relational with missing pieces.
11. As a user, I want document indexes shown and manageable for a collection, so that index work happens next to the data.
12. As a user, I want each connection and object to show whether an agent can reach it, so that I know what I have exposed.
13. As a user, I want to change agent access from the tree, so that exposure is one deliberate action and not a global switch.
14. As a user, I want the catalog to be available immediately after a restart, so that the first minute is not spent waiting for schemas again.
15. As a user, I want the restored catalog to refresh in the background, so that it is correct without me doing anything.
16. As a user, I want the catalog to update after I run DDL, so that a table I just created is in the tree.
17. As a user, I want to switch to another database on the same connection and get its own catalog, so that caches do not mix.
18. As a user, I want a DDL viewer that shows the server's own definition, formatted and copyable, so that I can take it elsewhere.
19. As a user, I want the DDL viewer to say when a definition cannot be generated, so that an empty tab does not look broken.
20. As a user, I want generated SELECT, INSERT, UPDATE, DELETE and DDL statements for an object, so that I do not type boilerplate.
21. As a user, I want generated statements to open in an editor tab, so that I can adjust them before running.
22. As a user, I want an object editor with sub-tabs for columns, indexes, constraints and options, so that related changes are made together.
23. As a user, I want the Schema change list to be derived from my edits, so that I see exactly what will change and can remove an entry.
24. As a user, I want a DDL preview that matches what will be applied, so that I approve the real statements.
25. As a user, I want a confirmation showing the full DDL before applying, so that a destructive change is not a surprise.
26. As a user, I want changes applied in a transaction where the database supports transactional DDL, and statement by statement where it does not, so that apply matches the server's rules.
27. As a user, I want a failed apply to keep my pending edits, so that I can fix the error instead of re-entering everything.
28. As a user, I want cleanup statements to run only when the change needs them, so that a failed rebuild does not leave the object broken.
29. As a user, I want to create tables, views, indexes, schemas and other supported objects from a dialog, so that new objects start from a form.
30. As a user, I want the create dialog to offer only the object kinds the driver supports, so that I do not fill a form that cannot be applied.
31. As a user, I want rename, drop and truncate to ask before acting, with cascade offered where it applies, so that I do not delete data by accident.
32. As a user, I want to be warned when closing or refreshing an editor with pending changes, so that my work is not lost silently.
33. As a user, I want to see users, roles and their grants in one grid, so that I can review privileges without writing queries.
34. As a user, I want privilege changes previewed before they apply, so that a revoke is deliberate.
35. As a user, I want global privileges and object privileges distinguished, so that I do not confuse a server-wide grant with a table grant.
36. As a user, I want to see server sessions and locks, with the blockers of a waiting session, so that I can find what is holding a lock.
37. As a user, I want to cancel a query or terminate a session from that view, with a confirmation and a second confirmation for my own session, so that I do not disconnect myself by mistake.
38. As a user, I want the session list to refresh only while it is visible, so that a background panel does not poll the server.
39. As a user, I want server properties, database sizes and table statistics in the same place, so that a quick health check needs no query.
40. As a user on a read-only connection, I want object and administration actions disabled rather than failing, so that the interface tells the truth.
41. As an agent-connected user, I want the Explorer to mark what an agent can reach, so that I can check exposure at a glance.
42. As a contributor, I want node paths to identify every node uniquely, so that loading, selection and persistence do not depend on display names.
43. As a contributor, I want catalog loading and invalidation to be engine responsibilities, so that the UI only renders state and returns actions.

## Implementation Decisions

- Core defines Nodes and Node paths, the catalog model (schemas and their object kinds, columns, indexes, constraints, routines, sequences, triggers and types where the driver has them), the schema description used by editors, the Schema change list and the diff that derives it, DDL generation templates and placements, and the administration models for users, roles, grants, sessions, locks, server properties, sizes and table statistics.
- The driver interface supplies browsing children and counts, introspection, object references and DDL. Object editing is optional and declared through the object-editing descriptor, so the engine and the app enable or hide actions from the descriptor rather than from the driver name.
- The engine answers explorer requests asynchronously: children of a node with counts, filter results, catalog loads per schema, object definitions and DDL, generated statements and administration reads. Each request is an Operation that can be cancelled and whose failure is reported to the node that asked.
- The catalog cache is held per Session and per database. Loading is lazy per schema; a schema already loaded is served from the cache. The cache is persisted in the local database keyed by Connection and database with load times, restored at startup, and refreshed in the background. It is invalidated for the affected objects after a DDL statement, after a root node refresh and after a delete; a failed load leaves the previous cache entry in place.
- The app keeps explorer state in a snapshot rebuilt each frame from engine state: per-node children state (not loaded, loading, ready, failed with message), counts, connection badges, agent-access marks and the active filter. The tree widget renders rows and returns actions; the app performs them.
- The filter matches names case-insensitively at the loaded levels and shows matched branches; it never triggers a full catalog read.
- Context menus are computed per node kind: connection, folder, schema, table, view, column, index, routine, sequence, document collection, index, key and database each get their applicable actions, including open data, open DDL, edit object, generate SQL, create objects, rename, drop, truncate, refresh, agent access, and document index administration.
- The DDL viewer is a tab that loads the object's definition, formats it, allows copying and reports when the server cannot produce one. It follows the applied definition after a successful editor apply.
- Generated SQL uses the Dialect's templates and identifier quoting, includes the object's columns and placeholders where applicable, and opens in an editor tab rather than executing.
- Object editors open as tabs with sub-tabs per object kind. They load the current definition, keep a working copy, derive the Schema change list from the diff against the loaded definition, render the DDL preview from the change list, and refuse to apply when there are no changes or the change list has problems. The loaded definition is remembered so pending changes can be re-based and the DDL tab can follow an apply.
- Apply asks for confirmation with the full DDL, then runs the statements according to the Dialect's placements: inside a transaction when DDL is transactional, one by one otherwise, with creation and cleanup statements placed as the dialect requires. On failure the pending changes stay, the error is shown, and the DDL preview still matches the pending state.
- Pending-edit protection asks before closing the tab or refreshing the object and before reloading over pending changes. Read-only connections disable apply and create actions.
- Create dialogs are generated from the object kinds the driver declares and validate names and required fields before applying; success opens the new object and refreshes the parent.
- Explorer object actions rename, drop and truncate confirm first, offer cascade where supported and refresh the affected parent afterwards.
- Administration panels read through the driver's optional administration interface and are shown only for supported capabilities: users and roles with a grants grid, sessions and locks with a blocking tree, server properties, database sizes and table statistics. The sessions panel refreshes while visible at an interval that the user can change; cancelling a query and terminating a session confirm first, and terminating the user's own session asks a second time.
- The store keeps catalog persistence in the local database and clears entries for a Connection when its database list or identity changes.
- Widgets for the tree, object editors and administration take state and return actions; they perform no I/O.

## Testing Decisions

- App seam: the application driven headlessly with fake drivers, asserting on tabs, dialogs and requests. Prior art: the object test suite (open an editor, failed apply keeps pending changes, create table through the dialog, drop asks, generate SQL opens a tab, rename from the tree, truncate offers cascade, the DDL tab follows the applied definition, a failed reload is shown, closing or refreshing with pending changes asks, the apply confirmation shows the full DDL) and the administration test suite (users tab preview and apply, global privileges note, revoke and drop ask first, closing the users tab asks, object permission edits stay pending, read-only disables actions, terminating one's own session asks twice, sessions refresh only while shown, server properties, sizes and statistics, panels follow driver support, object permissions and statistics, ignored visual audits).
- Engine seam: catalog loading and invalidation, persistence and restore, per-database separation, error handling and node failures. Prior art: the catalog tests (schema list and schema load, a load before the session is connected waits, a load without a session is ignored, DDL invalidates, root refresh invalidates, the catalog is persisted and restored, another database's catalog loads, a connection error fails the session), plus the engine's object, apply and administration suites.
- UI widget seam: the tree widget with states, badges, filter and menus; the object editor with change list, preview and problems; the administration widgets with the grants grid and session actions. Prior art: the existing widget test suites and their on-demand visual audits.
- Driver seam: object editing, DDL generation and administration exercised through the Conformance and driver suites where the driver declares support. Prior art: the relational driver suites and their administration coverage.
- Read-only enforcement is tested at both the engine and the app seam, so a disabled action cannot be reached through a stale menu.

## Acceptance Criteria

- [x] The Explorer shows Connections and folders, expands lazily and never blocks the UI thread on a catalog read.
- [x] Every node has a unique path independent of its display name.
- [x] Nodes show children counts where the driver provides them and per-node states (not loaded, loading, ready, failed) with inline errors offering retry, edit and details.
- [x] The tree filter matches names across loaded levels and does not trigger a full catalog read.
- [x] Connection rows show their state and commit mode.
- [x] Context menus offer only actions that apply to each node kind, including connection, schema, table, view, column, index, routine, sequence, collection and key.
- [x] Table and view data and DDL open from the tree, and document indexes can be administered from a collection.
- [x] Agent access is marked on connections and objects and can be changed from the tree.
- [x] Document and key-value connections have tree shapes suited to their Families.
- [x] The Catalog loads lazily per schema, is persisted per Connection and database, is restored at startup and refreshes in the background.
- [x] The Catalog is invalidated after DDL, after a root refresh and after a delete, and separate databases never share entries.
- [x] A failed catalog load leaves the previous entry usable.
- [x] The DDL viewer shows the server's definition, allows copying, and reports when no definition can be produced.
- [x] Generated SELECT, INSERT, UPDATE, DELETE and DDL statements use the Dialect and open in an editor tab.
- [x] Object editors load the current definition and derive the Schema change list from the difference with the working copy.
- [x] The DDL preview and the apply confirmation show exactly the statements that will run.
- [x] Apply runs in a transaction where DDL is transactional and statement by statement otherwise, with cleanup statements placed as the dialect requires.
- [x] Apply is refused when there are no changes or the change list has problems.
- [x] A failed apply keeps the pending changes and shows the error.
- [x] Closing, refreshing or reloading over pending changes asks first.
- [x] Create dialogs offer only the object kinds the driver supports and validate before applying.
- [x] Rename, drop and truncate confirm first and offer cascade where supported.
- [x] Administration shows users, roles and grants, sessions and locks, server properties, sizes and table statistics, only where the driver supports them.
- [x] Privilege changes are previewed before applying, and global and object privileges are distinguished.
- [x] Sessions and locks show blockers, cancellation and termination confirm first, and terminating one's own session asks twice.
- [x] The sessions panel refreshes only while visible, at a configurable interval.
- [x] Read-only connections disable edit, create and administration actions instead of failing them.
- [x] Catalog loading, persistence and invalidation live in the engine, and the UI only renders and returns actions.
- [ ] Table and view nodes offer a "Select 100 rows" menu entry that opens a limited result. The action exists and the application handles it, but no menu emits it, so opening data has no row limit.
- [ ] The Explorer action row shows a count of connections. It offers new connection, new folder, refresh selected and the filter, with no count.
- [ ] Object editing and administration are exercised against every bundled relational database in CI. The app and engine suites use fake drivers, and live coverage depends on the shared services.
- [ ] Object editors cover every object kind of every driver. Coverage follows the object-editing descriptor, and some drivers declare fewer kinds than their server offers.
- [ ] The Explorer offers schema compare and ER diagrams from object nodes. Both are planned areas and absent.

## Out of Scope

- Schema compare, ER diagrams and data transfer, which are separate areas.
- Table data editing in the grid; the Explorer only opens data tabs.
- Saved tree layouts and per-schema favourites.
- Multi-server catalog search beyond the tree filter.

## Further Notes

This task records retroactively the work that built the area as it exists today. The unticked criteria are the known gaps: the old design document listed a "Select 100 rows" entry that the tree menu never gained, although the action and its handler exist; the connection header described as showing a count is implemented as an action row without one; live object-editing and administration coverage depends on the container services and is not part of CI; object kinds offered by editors follow each driver's descriptor and are narrower than some servers allow; schema compare and ER diagrams belong to later areas. The design document's schema tree section also described snapshot comparisons for the tree widget; the tests use interaction checks and on-demand visual audits instead.
