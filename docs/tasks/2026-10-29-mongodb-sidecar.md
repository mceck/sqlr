---
status: to-do
---

# MongoDB as a Sidecar driver

## Specs

- [Drivers](../specs/drivers.md)
- [Architecture](../specs/architecture.md)

## Problem Statement

The app binary is about 146 MB in release. The MongoDB driver is by far its heaviest database dependency (the `mongodb` crate alone is several times larger than the Redis or MySQL clients), yet most users never connect to MongoDB. The Sidecar drivers DuckDB and SAP HANA already keep their weight out of the app, but the sidecar protocol only serves the Relational Family, so MongoDB, a Document driver, cannot follow them.

## Solution

MongoDB becomes a Sidecar driver, installed from the Driver Manager like DuckDB, and leaves the app binary. To make that possible the sidecar protocol grows to version 1.1.0, additively, so a Sidecar driver can belong to the Document Family and carry every Document operation the app uses today. A user with saved MongoDB Connections who has not installed the driver sees a clear error with an invitation to install it, and nothing is downloaded until they click. Everything MongoDB did before keeps working, identically, once the driver is installed.

## User Stories

1. As a user who never touches MongoDB, I want the app binary to be smaller, so that the download, install and launch cost nothing for a database I do not use.
2. As a MongoDB user, I want to install the MongoDB driver from the Driver Manager with one click, so that I get it without building anything.
3. As a MongoDB user with saved Connections, I want connecting without the driver to tell me it is missing and open the Driver Manager on `mongodb`, so that I know exactly what to do.
4. As a privacy-minded user, I want nothing downloaded until I click Download and install, so that the app never fetches anything on its own.
5. As a MongoDB user, I want queries in the Mongo shell subset, result paging, the JSON/document views and cancellation to behave as they did when the driver was built in, so that moving to the sidecar costs me nothing.
6. As a MongoDB user, I want to replace, insert and delete a document by `_id`, see read-only views as read-only, and see a collection's indexes, so that document editing and the index tools keep working.
7. As a MongoDB user, I want Database tools, Data transfer (import into and export from collections) and the MCP tools to work on a Connection served by the sidecar, so that no feature is lost.
8. As a MongoDB user, I want a driver crash to fail only my MongoDB connections, so that the app keeps running, as it does for every Sidecar driver.
9. As a MongoDB user, I want my Connection form, URL scheme, TLS settings and saved secrets unchanged, so that existing Connections work without editing.
10. As a driver author, I want to write an Elasticsearch, Redis or other Document or KeyValue driver as a sidecar later with the same SDK, so that the protocol extension is not Mongo-only.
11. As a driver author, I want the protocol spec to document the Document requests and the Doc chunk frame precisely, so that I can implement them in any language.
12. As a maintainer, I want an older sqlr (protocol 1.0) and an older driver (1.0) to keep working against each other, so that the protocol change breaks nobody.
13. As a maintainer, I want the release to publish `sqlr-driver-mongodb` archives for every platform with checksums, so that the Driver Manager can download them.
14. As a maintainer, I want the real size saving measured, so that the claim in the specs is a number, not a guess.

## Implementation Decisions

**Protocol 1.1.0 (additive).**

- `PROTOCOL` becomes `1.1.0`. The major version is unchanged, so the handshake still accepts 1.0 drivers; a driver on 1.0 simply never receives Document requests. A driver whose manifest declares the Document Family requires a Welcome with protocol ≥ 1.1; the client refuses to connect it otherwise.
- The "Scope" paragraph of the spec changes from "serves relational drivers" to "serves Relational and Document drivers"; KeyValue stays out of scope.
- New requests, each answered by `Unsupported` from a driver without a document interface: running a statement against a database (the `run` of the Document interface), fetching a page of documents from a document stream, closing a document stream, replace, insert and delete by `_id`, "is this target a view", "list the indexes of a target", and "set read-only". They carry the connection id and, where the Document interface takes one, the target.
- Running a statement answers with one of the four `DocExecution` outcomes: a document stream (stream id), a row stream (stream id and columns, then fetched with the existing `Fetch`), a write outcome (matched, modified, inserted, deleted, ids), or a single value. Documents and values use the existing Doc value model; the write ids and values cross the pipe in the Doc chunk value encoding, not as ad hoc MessagePack.
- Document pages travel in **document chunk frames** (type `0x03`, already reserved), sent before the control reply of the fetch, exactly as result chunks precede `FetchDone`. The Doc chunk encoding is unchanged. A chunk that would pass the 64 MiB frame limit is split in halves until every part fits; a single document larger than a frame fails the fetch with a message in the style of the row case.
- The synchronous methods of the Document interface (language, statement splitting, classifying, the open-target text and the target of a node) cannot cross an async pipe. The sidecar client implements them locally from the core types, selected by the language the manifest declares (the Mongo shell parser and its formatting already live in the core). The target of a node is derived from the node path and a field of the node the driver returns in `Browse`, never from an extra round trip.
- `set_read_only` is a request, sent when the app toggles a Connection's read-only flag and applied by the driver to the Connection's later writes; the Connection's initial value still arrives in `Connect`.
- Cancel, idle timeout, crash handling and secrets handling are unchanged. Secrets still cross the pipe only inside `Connect`; the readable form of a document request never prints the statement text, ids or document contents (consistent with the existing rule for `Execute`).
- The serve loop in the sidecar layer serves Document requests for any driver whose `Connection::document()` is present, and the driver SDK re-exports whatever a Document driver needs (target, executions, streams, Doc values) with doc comments in the vocabulary of the spec; the doc-comment lint on those crates stays green.
- The `0x03` frame, previously "reserved, protocol 1 sends none and the app ignores it with a warning", becomes a regular frame in 1.1: a 1.0 client still ignores it, which is why a Document driver is never started by a client that negotiated 1.0.

**The driver.**

- `crates/sqlr-driver-mongo` moves with `git mv` to `drivers/sqlr-driver-mongo`, becomes a workspace member there like DuckDB and HANA, gets a three-line `main` that hands the driver to the SDK's run function, and keeps compiling in process so the conformance suite can run it both ways. It has its own version, aligned with the release version like the other Sidecar drivers, and a package entry.
- Its manifest, connection form, URL scheme `mongodb`, TLS behaviour, timeouts, shell subset and EJSON handling do not change. The driver id stays `mongodb`.
- `sqlr-driver-mongo` is removed from `sqlr-app`'s dependencies and from the Bundled registration at startup. The `mongodb` Known database becomes installable (a Sidecar driver the release publishes) instead of bundled.
- Anything in the app or engine that reached into the mongo crate directly (types, parsers) must go through `sqlr-core` or the driver interface. The `sqlr-app` source mentions of "mongo" are Family-driven or id-driven and keep working because the driver id and Family are unchanged; each one is checked, not assumed.

**The app.**

- Connecting a Connection whose driver is not installed already offers the install instead of failing (the connect path checks the driver registry first). The decision for MongoDB is the same behaviour: the connect error names the MongoDB driver, a button opens the Driver Manager on `mongodb`, the download starts only on click. The task confirms by test that the Connection stays saved and untouched, and adds the wording for a Known database that moved from bundled to installable.
- Tools that run outside a tab (Database tools, Data transfer, MCP tools) obtain a Document Connection through the engine as before, so they work unchanged on a sidecar-backed Connection; the task verifies each by test or by the sidecar conformance run rather than assuming it.
- Elasticsearch and Redis stay Bundled; they are not touched.

**Release and tooling.**

- The release's driver list gains `mongodb`: the packaging scripts build and pack `sqlr-driver-mongodb_<version>_<os>-<arch>.tar.gz` for every platform with the others, listed in `SHA256SUMS.txt`. The packaging script and the release workflow are updated, and the coverage script treats the moved crate like the other drivers.
- `CLAUDE.md`'s crate table moves the driver out of `sqlr-driver-*` crates and notes that `drivers/` holds the sidecar drivers; the dependency check script is updated for the new location.
- Real size is measured: a release build before and after, recorded in the spec's Size paragraph (the claim "under 75 MB" is checked, not assumed). The measurement is the figure the spec cites, with the build command used.

**Milestones** (plan in this order; at most three tasks each):

1. *Protocol.* Core types and messages for the Document requests and results; the client's Document interface over the pipe, including the locally implemented synchronous methods; the serve loop's Document side and the SDK re-exports.
2. *Driver.* Move the crate, add `main`, run the Document conformance in process and through the sidecar client against the MongoDB test service; package it.
3. *App.* Remove the dependency and the registration, Known database becomes installable, the install prompt, release workflow and docs, size measurement, spec and `CLAUDE.md` updates.

## Testing Decisions

- **Seam 1, the highest: the conformance suite and the sidecar client against a real driver.** The `sqlr-driver-tests` suite today is relational. It gains a Document section (run a statement of each outcome kind, page documents, write by `_id`, view detection, indexes, read-only, cancel), run first in process and then through the sidecar client, which must report the same passes, exactly as the spec requires for relational drivers. The MongoDB driver runs it against the shared MongoDB test service.
- **Seam 2: the protocol layer with a fake.** Frame, message and chunk round trips, handshake negotiation (1.0 peer, 1.1 peer, a Document driver refused by a 1.0 Welcome), oversized-chunk splitting, malformed frames, and "Unsupported" for a relational driver receiving a Document request. These use `FakeDriver` from `sqlr-testkit` (extended with a Document fake) and pipe-backed tests like the existing sidecar tests.
- **Seam 3: the app.** `egui_kittest` and the engine's test harness cover: connecting a Mongo Connection with no installed driver shows the install invitation and downloads nothing; once the driver is registered the same Connection connects; the Known databases dialog lists MongoDB as installable.
- Prior art: the sidecar crate's client/server tests, the DuckDB and HANA sidecar tests under `drivers/*/tests`, the Mongo driver's own tests in `crates/sqlr-driver-mongo`, and the app's driver-install prompt tests.
- Per the project's TDD rule each piece starts with a failing test. Implementers run focused tests only; the full workspace suite runs once at the end of each milestone.

## Acceptance Criteria

- [ ] The sidecar protocol is 1.1.0 and the spec documents the Document requests, the document chunk frame, and the negotiation rules.
- [ ] A Document driver written with the SDK serves Document requests; a relational driver answers them with `Unsupported`.
- [ ] A 1.0 driver and a 1.0 client still interoperate; a Document driver is refused when the Welcome is below 1.1.
- [ ] The conformance suite has a Document section and the MongoDB driver passes it in process and through the sidecar client with identical results.
- [ ] `sqlr-driver-mongo` lives in `drivers/`, is no dependency of `sqlr-app`, and is not registered at startup.
- [ ] MongoDB statements, paging, editing by `_id`, views, indexes, cancellation, Database tools, Data transfer and MCP tools work on a Connection served by the installed sidecar.
- [ ] A saved MongoDB Connection without the driver installed offers the install, and nothing downloads until the user clicks.
- [ ] The `mongodb` Known database is installable; the release workflow and packaging scripts publish `sqlr-driver-mongodb_<version>_<os>-<arch>.tar.gz` with checksums.
- [ ] `cargo tree -d | grep egui` prints nothing, `cargo tree -i openssl-sys` finds no match, clippy passes with `-D warnings`, and the full suite passes with the container services.
- [ ] `docs/specs/drivers.md` and `docs/specs/architecture.md` describe what was built, `CLAUDE.md`'s crate table is current, and the measured release size before and after is recorded.

## Out of Scope

- Moving Elasticsearch or Redis to sidecars; the KeyValue Family over the protocol.
- Changing MongoDB behaviour, its form, the Mongo shell subset or its features.
- A distribution channel beyond the existing GitHub release and Driver Manager.
- Automatic download of the driver, or any background check for it.
- Reducing the size of other crates or changing build profiles.

## Further Notes

- No other task is a dependency. The three milestones are delivered in order, because the driver needs the protocol and the app change needs the installable driver.
- The protocol extension is deliberately general (Document, not Mongo) so Elasticsearch can follow as a sidecar without another protocol change.
- The size saving was only estimated from rlib sizes before this task; the real figure comes from the measurement in the acceptance criteria.
