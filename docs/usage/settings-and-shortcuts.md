# Settings and shortcuts

Open Settings with ⌘, (or the sqlr menu). A search field at the top finds a setting by its name, by what it does, or by a related word: typing `zoom` finds **UI scale**.

## Sections

| Section | What you can change |
|---|---|
| General | Whether a script stops at its first error, whether to check for updates at startup, how long finished operations stay in the Operations list |
| Appearance | Theme (System, Light or Dark), UI scale, accent color |
| Editor | Font size, line numbers, soft wrap, autocomplete behavior, the SQL formatter |
| Result grid | Rows per fetch, the row cap for Fetch all, the text shown for NULL |
| Data formats | How dates, times, timestamps, numbers, booleans, binary values and JSON look in cells |
| Keymap | Every shortcut, with presets |
| Connections | A session per editor tab, connect timeout, read-only and confirmation switches |
| Drivers | Extra properties for each driver, how long an idle driver process is kept |
| Agents | Enable the loopback MCP server, port, status, endpoint, token rotation, Copy MCP config |
| Terminal | Shell command, working directory, font size, scrollback; the font size applies immediately, the rest on Restart |

**Open settings folder** takes you to the files described below, in case you prefer to read or back them up directly.

### Layers

Many settings can be set at three levels. The global level applies everywhere, a profile can override it for all connections that use it, and a single connection can override that. The most specific level wins.

Safety settings are the exception. Read-only, Confirm writes and Confirm destructive statements can be switched on at a lower level, but never off. A connection can be stricter than your global settings, and never looser.

### Updates

Once a day, at launch, sqlr asks GitHub whether a stable release newer than the running one exists. Nothing is downloaded until you choose **Install**; **Skip this version** remembers that release, and closing the prompt asks again after the next daily check. The archive is checked against the release's `SHA256SUMS.txt` before it replaces anything.

On macOS **Install** replaces the `.app` bundle, asking for the administrator password when the bundle is in `/Applications`; on Linux it replaces the binary when its folder is writable. On Windows, and when sqlr cannot write its own folder, the same button opens the release page. Turn **Check for updates at startup** off in **Settings → General**; a check that cannot reach GitHub is never reported and runs again on the next launch.

**Check for updates now** in **Settings → General**, **Help → Check for Updates…** and the command palette ask immediately, even when the automatic check is off, and report both a failure and "you are up to date".

### Data formats

These control display only. The database keeps the value it has. You can choose date, time and timestamp patterns, digit grouping (none, space, underscore and so on), how many decimals to show for floating-point numbers, how booleans appear (`true / false`, `1 / 0` or `✓ / ✗`), whether binary values are hex, Base64 or UTF-8 text, and whether JSON in a cell is shown compact or pretty. Turn on **Use these formats for export** to have exported files follow them.

### Driver properties

Under Drivers you can pass extra properties to a driver for every connection that uses it. For PostgreSQL that includes `application_name`, `statement_timeout` and `search_path`. For MySQL, `sql_mode`, `time_zone` and `wait_timeout`. For SQLite, `foreign_keys`, `journal_mode`, `busy_timeout` and `cache_size`.

## Agents and terminal

**Settings → Agents** turns on the local MCP server. It is off by default and listens
only on `127.0.0.1`. Enabling it exposes no connection: set **Agent access** to Read or
Write on each connection. The bearer token lives in the encrypted credential vault, protected by the OS keychain; **Rotate token**
revokes it and disconnects agents. **Copy MCP config** copies a ready client
configuration containing the token.

**Settings → Terminal** sets the shell command, working directory and scrollback. The
shell command is not visible to agents and is never logged. Changes apply on Restart;
opening the terminal starts the shell lazily and hiding it keeps it running. See
[Agents and the terminal](agents.md).

## Keyboard shortcuts

Shortcuts below use the default keymap, with macOS keys. On Windows and Linux use Ctrl where you see ⌘.

### Running

| Action | Shortcut |
|---|---|
| Run statement or selection | ⌘↵ |
| Run script | ⇧⌘↵ |
| Stop the running query | Esc |
| Explain | ⌥⌘E |
| Explain Analyze | ⌥⇧⌘E |

### Tabs and navigation

| Action | Shortcut |
|---|---|
| New query tab | ⌘T |
| New query tab on the current connection | ⌘N |
| Close tab | ⌘W |
| Next tab, previous tab | Ctrl+Tab, Ctrl+⇧Tab |
| Open an object | ⌘P |
| Command palette | ⇧⌘P |
| Apply the object editor's changes | ⌘S |
| Rename the selected item in the explorer | F2 |
| Show operations | ⇧⌘O |
| Settings | ⌘, |
| Terminal | Ctrl+Backtick (every platform) |

### Editing

| Action | Shortcut |
|---|---|
| Find, find next, find previous | ⌘F, ⌘G, ⇧⌘G |
| Find and replace | ⌥⌘F |
| Toggle line comment | ⌘/ |
| Duplicate line or selection | ⌘D |
| Format SQL | ⌥⌘L |
| Save the selection (or the whole editor) as a snippet | ⇧⌘S |
| Complete | Ctrl+Space |
| Undo, redo | ⌘Z, ⇧⌘Z or ⌘Y |
| Indent, outdent | Tab, ⇧Tab |
| Delete the selected rows in a result | ⌘⌫ |

The full list of commands and their current keys is in **Help, Keyboard Shortcuts**. It always reflects your own changes.

### Changing shortcuts

In Settings, open Keymap. The Keymap section lists every command with its keys and lets you rebind them. Start from a preset: the default **sqlr** keymap, or **VS Code**, which moves a few commands to the keys VS Code uses. sqlr stores only what differs from the preset, in `keymap.toml`.

## Where things are stored

| What | Where |
|---|---|
| Connections (without passwords) | `connections.toml` in the config directory |
| Global settings | `settings.toml` in the config directory |
| Profile and connection overrides | `overrides.toml` in the config directory |
| Profiles | `profiles.toml` in the config directory |
| Keymap | `keymap.toml` in the config directory |
| Skipped update and last check | `updates.json` in the config directory |
| Installed drivers | `drivers/<id>/<version>/` in the data directory |
| Passwords, SSH secrets and MCP token | encrypted `secrets.vault` in the config directory; the OS keychain holds its encryption key under service `sqlr` |
| History and snippets | `sqlr.db` in the data directory |
| Logs (newest fourteen daily files) | `logs/sqlr.log.<date>` in the data directory |
| Local incidents (twenty files or thirty days) | `diagnostics/<incident-id>.json` in the data directory |

On macOS, both directories are inside `~/Library/Application Support/dev.sqlr.sqlr/`. On Linux, the config directory is `~/.config/sqlr/` and the data directory is `~/.local/share/sqlr/`. Use `--config-dir` and `--data-dir` to put them somewhere else.

Help → Diagnostics lets you select a log level for this session; it resets to `info` on restart. `RUST_LOG` takes precedence. General preferences and Diagnostics share **Include SQL in logs and reports** (`diagnostics.include_sql`, global only, default true). See [Troubleshooting](troubleshooting.md) for privacy, retention and sharing reports.

## Performance you can count on

sqlr is built to stay light, and the numbers below are checked on every change:

| What | Target |
|---|---|
| Start to a usable window | under 1 second |
| Memory while idle with a couple of connections open | under 150 MB |
| Scrolling a grid with over a million rows | 60 frames per second |
| Autocomplete popup, opening a table | under 50 ms |
| Size of the application | under 50 MB |

Details are in [Performance budgets](https://github.com/mceck/sqlr/blob/main/docs/tech/performance-budgets.md).
