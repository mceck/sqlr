# Getting started

## Install

On Linux and macOS one command installs the latest release for your system:

```sh
curl -fsSL https://raw.githubusercontent.com/mceck/sqlr/main/scripts/get-sqlr.sh | sh
```

It picks the build for your architecture (Linux x86_64 or aarch64, macOS Apple Silicon or Intel), checks it against the release checksums and installs it: on Linux to `~/.local/bin/sqlr`, on macOS to `/Applications/sqlr.app`. Set `SQLR_VERSION=0.2.0` for a specific version, or `SQLR_PREFIX=/usr/local` to change the Linux prefix. Windows builds and AppImages are on the download page.

Or you build sqlr from source with a recent Rust toolchain (1.98 or newer):

```sh
cargo build --release -p sqlr-app
cargo run --release -p sqlr-app
```

On macOS you can wrap the release binary in a normal app:

```sh
scripts/package-macos.sh
open dist/sqlr.app
```

That gives you `dist/sqlr.app` with its own Dock icon. Drag it to `/Applications` to keep it. The bundle is signed ad hoc, which is fine for your own machine. To run it on other Macs, sign it with your Developer ID using `--sign "Developer ID Application: ..."`.

Command line options:

```text
sqlr --config-dir <path>   read connections, profiles and settings from <path>
sqlr --data-dir <path>     keep history, snippets and logs in <path>
sqlr --version
sqlr --help
```

The two directories make it easy to keep a second, separate setup, for example one for work and one for personal projects.

## Try it without a server

When no connection is open you see the welcome screen. It shows a card for each database type, your recent connections and a button called **Open demo database**.

The demo is a small SQLite database with sample tables, created the first time you press the button. It is a good place to try everything in this guide before you point sqlr at a real server.

## Your first query

1. Press **Open demo database**. The connection appears in the explorer on the left.
2. Expand it, then expand the tables. Double-click a table to open its data in a tab.
3. Press ⌘T to open a new query tab on the same connection.
4. Type a query, for example `SELECT * FROM customers LIMIT 20;` (use a table name from the explorer).
5. Press ⌘↵. The statement under your cursor runs and the rows appear below.

If you select some text first, ⌘↵ runs the selection. To run everything in the editor as a script, press ⇧⌘↵.

## Your first real connection

Press **New connection** in the explorer header, or use ⌘⇧P and type "New connection". Choose your database, fill in the form and press **Test**. Test tells you the server version and how long the round trip took, so you know the details are right before you save. The full story, including SSH and TLS, is in [Connections](connections.md).

## The command palette

Two palettes get you around quickly:

- **⌘P** opens any object by name. It searches the tables and views of every connection you have, so `usr` finds `public.users` wherever it lives.
- **⌘⇧P** runs any command: New query tab, Export result, Compare schemas, Toggle soft wrap and so on. It also searches your snippets and your query history.

If you only remember one shortcut, remember ⌘⇧P.
