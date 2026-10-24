# sqlr

sqlr is a lightweight, universal database client in native Rust + egui. No webview. Open source, MIT.

- Product design: `docs/spec/sqlr-design.md`

## Workspace (`crates/`)

| Crate | Role | May depend on |
|---|---|---|
| `sqlr-core` | domain types, columnar chunks, driver traits | nothing internal |
| `sqlr-driver-*` | one driver per database (`tokio-postgres`, …) | `sqlr-core` |
| `sqlr-engine` | tokio runtime off the UI thread: sessions, explorer/catalog cache, results, history, installed sidecar drivers | `sqlr-core`, `sqlr-store`, `sqlr-registry`, `sqlr-sidecar` |
| `sqlr-sidecar` | sidecar protocol, `SidecarDriver` client, `serve` loop | `sqlr-core` |
| `sqlr-registry` | driver-file installs, versions and review (no UI) | `sqlr-core`, `sqlr-sidecar` |
| `sqlr-driver-sdk` | write a driver as a sidecar: re-exports the API, `run` | `sqlr-core`, `sqlr-sidecar` |
| `sqlr-driver-tests` | conformance suite for drivers (dev-dependency only) | `sqlr-core` |
| `sqlr-store` | connections TOML, encrypted credential vault with an OS-keychain-protected key, settings | `sqlr-core` |
| `sqlr-ui` | egui widgets (grid, tree, forms) | `sqlr-core` |
| `sqlr-mcp` | loopback MCP server, tool registry, policy, `AgentHost` | `sqlr-core`, `sqlr-engine`, `sqlr-store` |
| `sqlr-terminal` | PTY session, vt100 parser, egui terminal panel | egui, portable-pty, vt100 |
| `sqlr-app` | binary: wires everything | everything |
| `sqlr-testkit` | `FakeDriver` for tests (dev-dependency only) | `sqlr-core` |

## Commands

```sh
make test                                                # Docker services + nextest (services stop at the end)
make test-fast                                           # no database services
make coverage                                            # llvm-cov report per crate; uses Docker services
cargo nextest run --workspace --profile containers       # needs the services below
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt
cargo run --release -p sqlr-app
```

`make test` starts every server in `docker-compose.test.yml` (PostgreSQL, MySQL, MariaDB,
MongoDB, Redis, Elasticsearch, SQL Server and Oracle) on the fixed test ports and runs the
nextest `containers` profile. No test starts a server per test: per-test databases isolate
the parallel tests, and the server-global tests take a cross-process
`sqlr_testkit::TestLock`. The support modules read `SQLR_TEST_*` variables (or
`sqlr_testkit::service` defaults); the data lives in Docker volumes, and
`make test-services-reset` recreates them from empty. `make test-fast` sets
`SQLR_SKIP_CONTAINER_TESTS=1` and needs no services.

To run `cargo test` or `cargo nextest` directly, start the services first with
`scripts/test-services.sh up` (or `make test-services`). On Docker Desktop for macOS set
`DOCKER_HOST=unix://$HOME/.docker/run/docker.sock`. The docker CLI is
`~/.docker/bin/docker`, which is not on `PATH`.

## Hard rules

- **egui versions:** `egui`, `eframe`, `egui_extras` and `egui_kittest` are pinned to `=0.36.2`, and `egui_dock` to `0.21.1`. Never allow two egui versions in one build (`cargo tree -d | grep egui` must print nothing).
- **No OpenSSL:** rustls only (`cargo tree -i openssl-sys` must find no match). Connections begin without TLS unless the driver configuration enables it.
- **UI thread:** it never does I/O or blocks. Database, keychain and credential-vault work runs on background workers. Never hold an `engine.result(q)` read guard while calling engine methods.
- **Secrets:** never written to `connections.toml`, to logs, or to `Debug` output.
- **Diagnostics:** `tracing::error!` means an incident: reserve it for unexpected failures. Expected server errors use `info` or `warn`; cancellation uses `debug`. Diagnostics are local only and never make network requests. SQL in logs and reports follows the single global `diagnostics.include_sql` setting. Never log arguments, result rows or parameter values.
- **Catalog SQL:** always parameterized. Identifiers in generated SQL go through `Dialect::quote_ident`.
- **TDD:** write a failing test first. UI tests use `egui_kittest` 0.36 (`build_ui` / `build_ui_state`; it has no `allow_missing_glyphs()`).

## Conventions

- Code, comments, docs and commit messages are in English. Chat with the user is in Italian.
- Work and commit directly on local `main`: no branches, worktrees, pushes or PRs.
- Commit messages are conventional: `feat(engine): …`, `fix(postgres): …`, `docs: …`.
