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
| Idle RSS (reported; enforcement deferred) | < 150 MB | Process resident memory, two connected SQLite databases |
| Release binary | < 50 MB | Stripped `sqlr` executable, decimal MB |
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

The user explicitly deferred further memory optimization on 2026-10-23. RSS is
still measured and reported, but does not currently fail CI. Set
`SQLR_ENFORCE_RSS_BUDGET=1` to enable its original 150 MB gate. With bounded
native icon surfaces, the last measured native RSS was 169.5 MB. Restore the
strict CI gate when the follow-up optimization is completed; other budgets
remain enforced.
