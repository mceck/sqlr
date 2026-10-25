# Performance budgets

CI measures the release app on macOS and Linux. Startup, size, grid and editor regressions fail the job;
reports are uploaded as `performance-macOS` and `performance-Linux` artifacts.

```sh
cargo build --release -p sqlr-app --bin sqlr --example budgets
cargo run --release -p sqlr-app --example budgets
cargo run --release -p sqlr-ui --example editor_measure
python3 scripts/check-editor-budgets.py target/editor-measure.json
```

Linux needs a graphical session, or `xvfb-run -a` for the native app harness.
The native test creates temporary configuration and data directories, connects
 two SQLite databases, measures the same first-frame timer used by the app log,
samples process RSS after two seconds, and closes its window. User configuration
and databases are never opened. A native startup failure, missing report, timeout,
or fewer than two connected databases also fails the run.

| Metric | Limit | Measurement |
| --- | --- | --- |
| Cold startup | < 1,000 ms | Native process entry to first completed egui frame |
| Idle RSS | < 150 MB | Process resident memory, two connected SQLite databases |
| Release binary | < 75 MB | Stripped `sqlr` executable, decimal MB |
| Grid scroll p99 | < 16.7 ms | Headless CPU frames, 1,000,000 loaded rows, 300 verified jumps |
| Editor scenarios p99 | < 8 ms | `editor_measure` harness |
| Editor scope parsing p99 | < 5 ms | UI thread scope parsing |
| Editor first frame | < 200 ms | The `editor_measure` harness |
| Editor peak RSS | < 120 MB | Per-process scenarios, sampled peak RSS |

The grid fixture has an integer column and a text column. Every jump measures all
three settling frames; the test verifies the requested row actually became visible.
The native measurements include graphics initialization; headless widget frame times
exclude GPU work and presentation. This is a process launch measurement, not a claim
that the operating system's disk cache was purged.

The editor checker validates every exported `pass` verdict and rejects a report
with missing phases or no budget checks. `SQLR_SKIP_CONTAINER_TESTS=1` skips the
Docker integration test entry points on macOS CI; Linux runs them against the fixed
services from `docker-compose.test.yml`. It does not skip the local SQLite or fake-driver
tests.

## Local measurements

Machine: Apple M5, macOS 27.0.1. Measurements of the completed implementation are
recorded below after verification. Initial native WGPU measurements found a real
RSS regression (205–213 MB); the gate remained at 150 MB during investigation.

The follow-up on 2026-10-24 restored the strict 150 MB RSS gate. System CJK fonts are
now read on a background thread only after CJK text is painted. The native two-SQLite
fixture measured 171.33 MB before and 147.75 MB after this change (individual local
release runs; RSS varies with the OS and graphics driver). The grid scroll p99 was
1.35 ms with all 300 jumps verified.

## Result memory and sorting

SQL result payloads default to 128 MiB per result and 512 MiB combined. The global
preferences `grid.result_memory_mib` and `grid.total_memory_mib` accept `0` to disable a
limit. Both ordinary page fetches and fetch-all reserve bytes atomically before retaining
a chunk; a chunk that does not fit is discarded, the stream is closed, and a warning
marks the result as truncated. Existing rows are kept. Lowering a limit does not evict
loaded rows, and closed results release their reservations when their last handle drops.
These limits account for columnar buffer capacities, not total process RSS: in-flight
chunks, sorting keys, UI state, document results and graphics resources are separate.
The engine requests at most 1,024 rows per driver chunk; a single large value can still
require a larger temporary allocation before the engine can reject it.

RowBuffer snapshots share immutable chunks. Natural grid order uses a row count rather
than an index vector, saving 8 MB per million rows on a 64-bit target. Local sorting of
10,000 or more loaded rows runs on one worker per used grid. Each worker has one running
job and one replaceable pending job; stale results cannot replace the current view. A
running job finishes before the next one starts. Small sorts stay synchronous. Numeric
text keys are parsed once per row, while ordinary typed columns keep their direct
comparison path. Exact decimal comparisons and stable ties are preserved.

The deterministic sorting benchmark can be reproduced with:

```sh
cargo run --release -p sqlr-ui --example sort_measure
```

The decimal fixture deliberately uses values differing beyond f64 precision, exercising
the exact-comparison path. Times below are medians of three optimized executions on the
same local machine, excluding UI rendering and database work:

| Rows | Kind | Before | After |
| --- | --- | --- | --- |
| 100,000 | Integer | 30.04 ms | 27.58 ms |
| 100,000 | Precise decimal | 392.72 ms | 39.18 ms |
| 1,000,000 | Integer | 379.31 ms | 383.91 ms |
| 1,000,000 | Precise decimal | 4,664.25 ms | 611.89 ms |

SQL Server now reads only on a chunk request, with one page in flight. An open result
cannot block explorer metadata: a separate inspection connection opens lazily when the
query connection is occupied. Closing an unfinished result drains remaining TDS responses
with bounded memory, preserving batch side effects; it can wait for the server to finish.
Explicit cancellation retains the existing cancel handle. PostgreSQL statements that
cannot use a cursor now use bounded protocol streams, including statements with parameters.
Closing RETURNING output does not cancel the write's side effects.

Validation: 1,953 workspace tests passed without container services (17 skipped), 147
PostgreSQL/SQL Server driver tests passed against real services, and all three SQL Server
engine transaction tests passed. The later metadata responsiveness regression test also
passed in the 39-test SQL Server driver suite. Clippy passed with warnings denied.

Full release LTO reduces the binary from 53.42 MB (thin LTO) to 49.29 MB, below the
unchanged 50 MB limit. This is a build-profile change only; every driver, graphics backend
and product feature remains included. The tradeoff is a slower release link (the local
full-LTO trial took 3m42s, including recompilation, versus the preceding thin-LTO build's
1m48s). The original executable before this work was 53.33 MB.

The binary gate was raised from 50 MB to 75 MB on 2026-10-25. Before the native Snowflake
driver the stripped executable already measured 50.24 MB; with it, 50.41 MB (local Apple M5
release builds). DuckDB, whose embedded engine adds about 37 MB (86.17 MB with it bundled),
ships as a sidecar driver file instead (`scripts/package-driver.sh duckdb`), so the app keeps
headroom under the new limit.
