# {{project-name}}

An sqlr driver. It runs as its own process (a *sidecar*) and talks to sqlr over stdin and
stdout, so it can be written, built and shipped separately from sqlr.

1. Fill in the manifest in `src/lib.rs`: the id, the name and the connection form fields.
2. Replace `MyConnection::execute`, `browse` and `cancel` with calls to your database.
3. Test it with the shared conformance suite: build a `sqlr_driver_tests::Target` for your
   database and call `relational_suite`. A driver is done when `report.failed` is empty.
4. `cargo build --release`, then in sqlr: Settings → Drivers → Install from file…

Rules of the protocol: stdout carries only protocol frames (never `println!`); log to
stderr, which sqlr writes to its log prefixed with the driver id; secrets reach `connect`
only, never log them. See `docs/writing-a-driver.md` and `docs/driver-protocol.md` in the
sqlr repository.
