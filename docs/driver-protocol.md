# Sidecar driver protocol, version 1.0.0

A *sidecar* driver is a separate executable that sqlr starts and talks to over its standard
input and output. This document is the contract; the implementation is
`crates/sqlr-sidecar` (`frame.rs`, `messages.rs`, `dialect.rs`) and the encodings of result
data are in `crates/sqlr-core/src/wire.rs`. Write a driver in Rust with
`sqlr-driver-sdk` (see `docs/writing-a-driver.md`) or implement this protocol in any other
language.

## Transport

Everything is a **frame**:

| bytes | field |
|---|---|
| 4 | `length`: big-endian `u32`, the size of everything after it (9 + payload) |
| 1 | `type` |
| 8 | `request id`: big-endian `u64` |
| … | payload |

A frame is at most **64 MiB** (`length` ≤ 67 108 864); a producer splits larger chunks.
A reader never allocates the length a header claims before the bytes arrive, and treats an
unknown type, a length below 9 or above the limit, or a pipe that closes inside a frame as a
protocol error.

| type | payload |
|---|---|
| `0x01` control | MessagePack (named fields) of a `Request` (sqlr → driver) or `Response` (driver → sqlr) |
| `0x02` result chunk | `wire::encode_chunk` bytes, sent before the control reply of a `Fetch` |
| `0x03` document chunk | `wire::encode_doc_chunk` bytes (reserved: protocol 1 serves relational drivers) |
| `0x04` log line | UTF-8 text, written to the app log with the driver id prefix |

**stdout carries frames only.** A driver logs to stderr, which sqlr copies to its log line by
line, prefixed with the driver id.

Requests carry a fresh id; every answer to a request carries the same id. Requests are
served concurrently, so a `Cancel` can arrive while an `Execute` is running.

## Handshake and versioning

sqlr sends `Hello { protocol, sqlr_version }`; the driver answers
`Welcome { protocol, manifest }`. Protocols are semver: a different **major** version makes
sqlr refuse the driver with `Driver <id> speaks protocol X, sqlr speaks Y`. The handshake
has 10 seconds to complete; after that the process is killed.

## Requests

| request | answer |
|---|---|
| `Connect { conn, config, secrets }` | `Connected { server_info, dialect, database_name, schema_path }` |
| `ConnectDatabase { conn, new_conn, database }` | `Connected` for `new_conn` |
| `Ping`, `Browse { path }` | `Pong(server_info)`, `Nodes` |
| `Execute { conn, sql, params? }` | `Rows { stream, columns }` or `Affected(n)` |
| `Fetch { stream, max }` | zero or more chunk frames, then `FetchDone { exhausted }` |
| `CloseStream`, `Disconnect`, `Shutdown` | `Ok` |
| `Cancel { conn }` | `Ok` once the cancel was issued |
| `Describe { table }`, `Catalog { schema }`, `TakeNotices` | `Table`, `Catalog`, `Notices` |

Any request may be answered by `Error { kind, message, position, hint }`, which maps one to
one to `sqlr_core::DriverError`. `kind` is one of `Connection`, `Auth`, `Syntax`, `Timeout`,
`Cancelled`, `Permission`, `Unsupported`, `Protocol`, `Other`.

**Secrets cross the pipe only inside `Connect`** and are never logged; sqlr's protocol log
prints their keys, not their values. A driver receives only the fields its manifest declares
`secret`.

### Streams

`Execute` of a query returns a stream id. `Fetch { stream, max }` asks for up to `max` rows:
the driver answers with chunk frames (each at most 64 MiB; a larger page is split) and then
`FetchDone`. `exhausted: true` means no more rows; sqlr then no longer sends `Fetch` or
`CloseStream` for it. `Disconnect` closes the connection's streams.

### Dialect

`Connected.dialect` is data, not code: quoting, keywords, functions, comment and string
syntax (`SyntaxSpec`), placeholder style (`?`, `$1`, `:1`), the limit clause
(`LIMIT`, `FETCH FIRST`, `TOP`), which path segments name a relation in `SELECT * FROM …`,
default schemas, the begin statement, read-only statements and transaction quirks. sqlr
builds its `Dialect` from it. A relational driver sets it; `serve` fills it from the
driver's own `Dialect` implementation by probing it.

### Cancellation and crashes

`Cancel` asks the driver to stop the connection's running statement. If a statement is
still running 5 seconds later, sqlr kills the process. A process that exits or breaks the
protocol fails only its own connections with a `Protocol` error; sqlr offers "Restart
driver". A sidecar is started on the first connection that needs it, serves any number of
connections, and is stopped 5 minutes after the last one closes.

## Result chunk encoding (`0x02`)

Little-endian numbers, LEB128 varints for counts and lengths.

```
version u8 (1)
ncols varint, per column: name, type_name (varint len + UTF-8), kind u8, hint u8,
                          origin u8 (0 none | 1 database?, schema?, table, column)
nrows varint
per column: validity bitmap (ceil(nrows/8) bytes, bit set = not NULL)
            values length varint, then the values of the non-NULL rows:
            Bool u8 · Int i64 · Float f64 · Text/Bytes varint len + bytes ·
            Mixed: tag u8 (0 null,1 bool,2 int,3 float,4 text,5 bytes) + value
```

Kinds: 0 Bool, 1 Int, 2 Float, 3 Text, 4 Bytes, 5 Mixed. Decimals, dates, UUIDs and the like
travel as Text, with the column's display hint (Decimal, Temporal, Uuid, …) saying what the
text holds. A decoder rejects every malformed encoding with an error: truncation, unknown
tags, invalid UTF-8, counts that no remaining bytes could back, trailing bytes.

## Conformance

`sqlr-driver-tests` is the reusable suite: connect, ping, browse, execute and stream, paging,
affected rows, syntax errors, cancel, a type round-trip matrix and (when declared) table
description for editing. A driver passes when `SuiteReport::failed` is empty; a sidecar must
pass the same checks as the in-process driver it wraps.

## Fuzzing

`fuzz/` holds `cargo fuzz` targets for the chunk decoders (`chunk_decode`) and the frame and
message decoders (`frame_decode`); they need a nightly toolchain
(`cargo +nightly fuzz run chunk_decode`). Without nightly,
`crates/sqlr-core/tests/wire.rs::random_mutations_never_panic` runs 20 000 seeded mutations.
