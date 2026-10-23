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
| `sqlr-store` | connections TOML, OS keychain secrets, settings | `sqlr-core` |
| `sqlr-ui` | egui widgets (grid, tree, forms) | `sqlr-core` |
| `sqlr-app` | binary: wires everything | everything |
| `sqlr-testkit` | `FakeDriver` for tests (dev-dependency only) | `sqlr-core` |

## Commands

```sh
cargo test --workspace                                   # Postgres tests need Docker (see below)
cargo nextest run --workspace --profile containers       # runs test binaries concurrently
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt
cargo run --release -p sqlr-app
```

`make test` starts the shared Postgres/MySQL/MariaDB services from
`docker-compose.test.yml` and runs the nextest `containers` profile; every test gets its
own database. `make test-services-reset` recreates them. Without `SQLR_TEST_*` variables,
tests fall back to testcontainers. `make test-fast` sets
`SQLR_SKIP_CONTAINER_TESTS=1` and needs no Docker.

The Postgres integration tests use testcontainers with Docker Desktop. Set `DOCKER_HOST=unix://$HOME/.docker/run/docker.sock`. The docker CLI is `~/.docker/bin/docker`, which is not on `PATH`.

## Hard rules

- **egui versions:** `egui`, `eframe`, `egui_extras` and `egui_kittest` are pinned to `=0.36.2`, and `egui_dock` to `0.21.1`. Never allow two egui versions in one build (`cargo tree -d | grep egui` must print nothing).
- **No OpenSSL:** rustls only (`cargo tree -i openssl-sys` must find no match). Connections begin without TLS unless the driver configuration enables it.
- **UI thread:** it never does I/O or blocks. All database and keychain work runs in the engine. Never hold an `engine.result(q)` read guard while calling engine methods.
- **Secrets:** never written to `connections.toml`, to logs, or to `Debug` output.
- **Catalog SQL:** always parameterized. Identifiers in generated SQL go through `Dialect::quote_ident`.
- **TDD:** write a failing test first. UI tests use `egui_kittest` 0.36 (`build_ui` / `build_ui_state`; it has no `allow_missing_glyphs()`).

## Conventions

- Code, comments, docs and commit messages are in English. Chat with the user is in Italian.
- Work and commit directly on local `main`: no branches, worktrees, pushes or PRs.
- Commit messages are conventional: `feat(engine): …`, `fix(postgres): …`, `docs: …`.
