# Redis in sqlr

`run_query` takes one Redis command line, for example `HGETALL user:42` or `SCAN 0 MATCH session:* COUNT 100`.

- The answer is a `reply` Result: a JSON form of the RESP value (nil, string, integer, array, map, double, boolean, error, verbatim). It has no pages and no `query_id`.
- `MULTI`, `EXEC`, `DISCARD`, `MONITOR`, `SUBSCRIBE`, `PSUBSCRIBE` and blocking commands (`BLPOP`, `BRPOP`, `BLMOVE`, …) answer `unsupported`.
- A command that times out answers `running` without an id. Do not send it again: it may still complete on the server.

Never use `KEYS *` on a real keyspace: use `SCAN` with `MATCH` and a `COUNT`, and stop after a few iterations, or `browse`, which groups keys by prefix and bounds the scan. Read large values in pages: `HSCAN`, `SSCAN`, `ZRANGE … LIMIT`, `LRANGE 0 99`, `XRANGE … COUNT n`, `GETRANGE`.

A command is a read only when Redis lists it as one. `DEL` and `UNLINK` delete; `FLUSHDB`, `FLUSHALL` and `SHUTDOWN` are destructive; module commands and scripts count as writes. Redis has no transactions here: writes apply at once.

The logical database comes from the Connection; `browse` lists databases, key prefix groups and keys.
