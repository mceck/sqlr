---
status: to-do
---

# Apply the Rust best practices across the workspace

## Specs

- [Architecture](../specs/architecture.md)
- [Drivers](../specs/drivers.md)

## Problem Statement

The `rust-best-practices` skill (Apollo's Rust handbook) was installed after most of sqlr was written. An audit of the workspace against it found that the code mostly follows it, but a set of practices is neither applied everywhere nor enforced, so new code can drift again:

- Clippy runs in CI with warnings denied, but the workspace configures no lints of its own: the lints the handbook asks to respect (`redundant_clone`, `needless_collect`, `unnecessary_wraps`, `map_unwrap_or`, `manual_let_else`, `needless_pass_by_value`) are off, and Clippy pedantic reports 70 redundant clones (45 in production code), 6 needless collects, 10 functions that always return `Ok`/`Some`, 59 values passed by value and never consumed, 39 `map(..).unwrap_or(..)` chains and 18 manual `let … else`.
- Lints are silenced with `#[allow(...)]` (30 in production code, 17 of them without any justification) instead of `#[expect(..., reason = "...")]`, so a silenced lint stays silenced after the code that triggered it is gone.
- Production code calls `.unwrap()` 87 times. About 30 are `std::sync::Mutex::lock().unwrap()` although the workspace already depends on `parking_lot`, whose locks cannot be poisoned; the others unwrap an `Option` right after the code checked it.
- 144 functions return `Result<_, String>`, including public interfaces of the core (Changeset preview), the store (connection import from other clients), the engine, the UI and the app. Callers cannot tell failures apart, and the error messages are scattered across format strings rather than defined once per module.
- Four `unsafe` blocks have no `// SAFETY:` comment.
- The crates that third-party Driver authors build on (the driver SDK and the core and sidecar crates it re-exports) have about 1,150 public items without documentation.

## Solution

Every practice in the handbook that the audit found missing is applied to the whole workspace and turned into a check that `cargo clippy --workspace --all-targets -- -D warnings` (already run locally and in CI) enforces, so drifting back fails the build:

- The workspace declares its lints once, and every crate inherits them.
- Silenced lints use `#[expect]` with a reason.
- Production code never calls `.unwrap()`; locks come from `parking_lot`.
- Fallible functions return typed errors defined with `thiserror`, one error type per module or area, whose messages are the ones users see today.
- Every `unsafe` block states why it is sound.
- The driver-authoring API (core, sidecar, driver SDK) documents every public item.

Users see no change: every error message keeps its current text and every behaviour stays as it is.

## User Stories

1. As a contributor, I want the workspace to declare its Clippy lints in one place, so that every crate follows the same rules without repeating them.
2. As a contributor, I want a redundant `.clone()` to fail the lint gate, so that unnecessary allocations do not creep into hot paths such as chunk handling and the grid.
3. As a contributor, I want an iterator collected only to be iterated again to fail the lint gate, so that intermediate allocations are caught at review time.
4. As a contributor, I want a function that always returns `Ok` or `Some` to fail the lint gate, so that signatures tell the truth about what can fail.
5. As a contributor, I want a value passed by value but only borrowed to fail the lint gate, so that functions borrow by default and callers do not clone to satisfy them.
6. As a contributor, I want `map(..).unwrap_or(..)` chains and hand-written `match`es that a `let … else` expresses better to fail the lint gate, so that `Option` and `Result` handling reads the same everywhere.
7. As a contributor, I want every silenced lint to carry a reason, so that the next reader knows why the rule does not apply there.
8. As a contributor, I want silenced lints in production code to use `#[expect]`, so that the compiler tells me when the silencing has become unnecessary.
9. As a contributor, I want `.unwrap()` in production code to fail the lint gate, so that a panic is always a deliberate, explained `expect` on an invariant, never an accident.
10. As a contributor, I want tests, examples, benches and the test-only crates to keep using `.unwrap()`, so that test code stays short.
11. As a user, I want a background worker that panicked once not to poison a lock and take down unrelated features, so that one bug does not cascade.
12. As a contributor, I want `std::sync::Mutex`, `RwLock` and `Condvar` to be refused by the lint gate, so that every lock in the workspace is a `parking_lot` lock with one locking idiom.
13. As a contributor calling a core, store, engine, UI, terminal, MCP or app function, I want a typed error instead of a `String`, so that I can match on the failure that matters to me.
14. As a contributor, I want each module's error messages defined once on its error type, so that changing a message is a one-line edit.
15. As a user, I want every error message I see in a toast, dialog, result tab or log to stay word for word as it is today, so that this refactor changes nothing for me.
16. As a contributor, I want errors from lower layers wrapped with `#[from]` or `#[source]`, so that the cause chain is preserved and `?` propagates without `map_err` noise.
17. As a contributor, I want the tests that asserted on error strings to keep asserting the same text through `to_string()`, so that the user-visible messages stay covered.
18. As a reviewer, I want every `unsafe` block to be preceded by a `// SAFETY:` comment, and the lint gate to refuse one that is not, so that soundness is argued where it is relied on.
19. As a third-party Driver author, I want every public item of the driver SDK and the core and sidecar crates it re-exports to be documented, so that I can write a Driver from the rustdoc alone.
20. As a maintainer of the driver API, I want an undocumented public item in those crates to fail the build, so that the driver-authoring documentation stays complete.
21. As a contributor, I want the lint policy, error-type policy and locking policy written in the Architecture spec, so that I know the rules before CI tells me.
22. As a maintainer, I want the CI command to stay `cargo clippy --workspace --all-targets -- -D warnings`, so that the gate is the same locally and in CI with no extra flags to remember.

## Implementation Decisions

### Workspace lints

- The root manifest gains a `[workspace.lints.clippy]` table and every member crate (including the two drivers under `drivers/`) sets `lints.workspace = true`. The crates outside the workspace (`fuzz`, `templates`, `tools`) are left alone, except that the driver template's code is fixed if it contains any of the patterns below, because driver authors copy it.
- Lints set to `deny` at the workspace level: `redundant_clone`, `needless_collect`, `unnecessary_wraps`, `needless_pass_by_value`, `map_unwrap_or`, `manual_let_else`, `manual_ok_or`, `undocumented_unsafe_blocks`, `allow_attributes_without_reason`, `disallowed_types`.
- Clippy pedantic is not enabled as a group; only the lints listed above are taken from it.
- `unwrap_used` is denied in production code only. Each crate that is not test-only (everything except `sqlr-testkit` and `sqlr-driver-tests`) enables it at its crate root for non-test builds, so unit test modules, integration tests, examples and benches are exempt. `expect` with a message that states the invariant stays allowed: the audit found the existing ones state real invariants (arena sizes, valid UTF-8 written by our own encoder, lengths already checked).
- A `clippy.toml` at the workspace root lists `std::sync::Mutex`, `std::sync::RwLock` and `std::sync::Condvar` as disallowed types, with `parking_lot` named as the replacement. This also applies to tests. `std::sync::OnceLock`, `LazyLock`, atomics, `Arc` and channels stay allowed.
- `unnecessary_wraps` and `needless_pass_by_value` keep Clippy's default of not flagging exported public API. Where a signature is imposed by a trait (driver traits, `egui` callbacks, `ToSql`), the fix is not forced: the site gets an `#[expect(..., reason = "...")]` naming the constraint.

### Silenced lints

- Every `#[allow]` in production code becomes `#[expect(lint, reason = "...")]`. The reason says why the rule does not apply (for `too_many_arguments`: what the arguments are and why grouping them would not help; for `large_enum_variant`: why boxing is not worth it).
- An `#[allow]` stays an `#[allow]` only where an expectation cannot hold on every target: the three shared integration-test support modules whose helpers each test binary uses a different subset of. They gain a `reason = "..."`.
- Where an `#[allow]` turns out to be unnecessary (the lint no longer fires), it is removed.

### Unwrap and locks

- Every `std::sync::Mutex`/`RwLock`/`Condvar` in the workspace, production and test code alike, becomes the `parking_lot` equivalent, and `.lock().unwrap()` becomes `.lock()`. Crates that do not depend on `parking_lot` yet add the workspace dependency.
- A `Condvar` wait loop (the grid's sort worker) uses `parking_lot::Condvar::wait(&mut guard)`, which takes the guard by mutable reference.
- An `Option::unwrap()` after a check becomes `let … else`, `if let`, a `match` or a restructuring that removes the check; when the value is truly guaranteed by a local invariant that the type system cannot express, it becomes an `expect` whose message states that invariant.
- Mutex poisoning was never handled on purpose anywhere, so moving to `parking_lot` changes no intended behaviour.

### Typed errors

- No function, method or trait method in the workspace returns `Result<_, String>` any more, in any crate including `sqlr-app`. Closures and local helpers inside one function are included: the rule is about signatures, wherever they appear.
- Each module (or small group of closely related modules) that produces errors gets one `#[derive(Debug, thiserror::Error)]` type, usually an enum named after the area (for example an import error for connection import from other clients, a changeset error for Changeset preview, an update error for the app's update check). A module whose failures have a single shape may use a struct error. Crates that do not depend on `thiserror` yet add the workspace dependency.
- Each variant's `#[error("...")]` reproduces exactly the text the code produced before, including interpolated values, so that every message shown in the UI or written to the log is unchanged.
- Errors from other crates or layers are wrapped with `#[from]` (when `?` should convert them) or `#[source]` (when a variant adds context), never flattened into a string. Where the old code formatted a lower error into its message, the new variant's message format keeps the same text, e.g. `#[error("cannot read {path}: {source}")]`.
- UI code that only displays an error calls `to_string()` at the point where it puts the message into a toast, dialog or label; error values themselves travel typed until then.
- Existing public types that already carry errors (`DriverError`, the store's and registry's error enums) are reused instead of duplicated where the failure belongs to them.
- `anyhow` stays only where the app wires its dependencies at startup and in the examples; those already return `anyhow::Result`.
- Error types do not include secrets, parameter values or result rows in their messages (the existing diagnostics rule).

### Unsafe

- The four `unsafe` blocks without a safety argument (process-group kill in the terminal PTY, two DuckDB FFI conversions, the memory-limit test setup in the engine's transfer tests) get a `// SAFETY:` comment that says why the call is sound. The existing comments that explain the DuckDB calls become the safety argument.

### Driver API documentation

- `sqlr-core`, `sqlr-sidecar` and `sqlr-driver-sdk` deny `missing_docs` at their crate root. Every public item reachable from these crates (types, traits, functions, methods, fields, enum variants, constants, modules) gets a `///` doc comment, or `//!` for modules.
- Doc comments say what the item is or does in terms of the glossary (Driver, Family, Capability, Dialect, Result chunk, Doc chunk, sidecar protocol) and add how or why only where it is not obvious. A field or variant whose name says it all still gets a short line; no doc comment restates the type signature.
- Other crates are not required to document every public item.

### Specs

- The Architecture spec gains a "Code quality gates" subsection describing the lint policy, the error-type policy, the locking policy, the unwrap rule and the driver-API documentation rule, plus a Decisions entry for each policy. The Drivers spec's section on the driver SDK states that the driver-authoring API is fully documented and that the build enforces it.

## Testing Decisions

- **Seam 1: the lint gate.** `cargo clippy --workspace --all-targets -- -D warnings` is the check for every rule that can be expressed as a lint: the workspace lints, the disallowed lock types, the unwrap rule, the safety comments and the missing docs. It must pass with no warnings. No test is written for a lint.
- **Seam 2: the existing test suite, unchanged.** This is a behaviour-preserving refactor; the existing unit, engine, widget and app tests are the regression net and must keep passing without being weakened. A test that asserted on an error `String` keeps asserting the same text through `to_string()` on the new error value, or matches the variant where that is what the test is about.
- **New tests for error messages.** Where a function changes from `Result<_, String>` to a typed error and no existing test covers one of its user-visible messages, a test is added first that pins the current message through `to_string()` on the error the function returns (written against the old `String` before the change, so it passes before and after). Prior art: the store's client-migration tests and the core's Changeset tests, which already assert on error text.
- **The no-`String`-error rule** has no lint; it is verified by searching production and test code for `Result<…, String>` in signatures, which must find nothing.
- Implementers run Clippy and the tests of the crates they touch; the full workspace suite (with container services) runs once at the end.

## Acceptance Criteria

- [ ] The root manifest declares `[workspace.lints.clippy]` with the deny list above, and every workspace member inherits it with `lints.workspace = true`.
- [ ] `clippy.toml` disallows `std::sync::Mutex`, `std::sync::RwLock` and `std::sync::Condvar`, and no crate uses them.
- [ ] `unwrap_used` is denied in the production code of every crate except `sqlr-testkit` and `sqlr-driver-tests`.
- [ ] Production code contains no `#[allow(...)]`; every silenced lint uses `#[expect(..., reason = "...")]`; the remaining test-support `#[allow]`s have a reason.
- [ ] Every `unsafe` block has a `// SAFETY:` comment.
- [ ] No function signature in the workspace returns `Result<_, String>`.
- [ ] Every new error type derives `thiserror::Error`, wraps lower errors with `#[from]` or `#[source]`, and reproduces the previous message text exactly.
- [ ] `sqlr-core`, `sqlr-sidecar` and `sqlr-driver-sdk` deny `missing_docs`, and `cargo doc` for them builds without warnings.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes.
- [ ] `cargo fmt --all -- --check` passes.
- [ ] `cargo tree -d | grep egui` prints nothing and `cargo tree -i openssl-sys` finds no match.
- [ ] The full workspace suite passes (`make test`).
- [ ] The Architecture and Drivers specs describe the code quality gates.

## Out of Scope

- Enabling Clippy pedantic or nursery as groups, and the pedantic lints not listed above (`must_use_candidate`, casts, `too_many_lines`, `missing_errors_doc`, `doc_markdown` and the rest).
- Requiring documentation of every public item in crates other than core, sidecar and the driver SDK.
- Snapshot testing (`insta`) and test renaming: the existing test names already describe behaviour.
- Changing any user-visible message, behaviour or performance characteristic.
- The crates outside the workspace (`fuzz`, `tools`), apart from fixing the driver template's code.

## Further Notes

- Implemented on a separate worktree branch and merged back into local `main` when done.
- The audit that motivated this task ran Clippy with `-W clippy::pedantic -W clippy::redundant_clone -W clippy::needless_collect`; its counts are in the Problem Statement and give the expected size of each step.
- `unnecessary_wraps` and `needless_pass_by_value` findings on functions whose signature is fixed by a trait or a callback type are expected and get an `#[expect]` with the reason; they are not counted as misses.
