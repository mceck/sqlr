# Settings and shortcuts

Open Settings with ⌘, (or the sqlr menu). A search field at the top finds a setting by its name, by what it does, or by a related word: typing `zoom` finds **UI scale**.

## Sections

| Section | What you can change |
|---|---|
| General | Whether a script stops at its first error, how long finished operations stay in the Operations list |
| Appearance | Theme (System, Light or Dark), UI scale, accent color |
| Editor | Font size, line numbers, soft wrap, autocomplete behavior, the SQL formatter |
| Result grid | Rows per fetch, the row cap for Fetch all, the text shown for NULL |
| Data formats | How dates, times, timestamps, numbers, booleans, binary values and JSON look in cells |
| Keymap | Every shortcut, with presets |
| Connections | A session per editor tab, connect timeout, read-only and confirmation switches |
| Drivers | Extra properties for each driver, how long an idle driver process is kept |

**Open settings folder** takes you to the files described below, in case you prefer to read or back them up directly.

### Layers

Many settings can be set at three levels. The global level applies everywhere, a profile can override it for all connections that use it, and a single connection can override that. The most specific level wins.

Safety settings are the exception. Read-only, Confirm writes and Confirm destructive statements can be switched on at a lower level, but never off. A connection can be stricter than your global settings, and never looser.

### Data formats

These control display only. The database keeps the value it has. You can choose date, time and timestamp patterns, digit grouping (none, space, underscore and so on), how many decimals to show for floating-point numbers, how booleans appear (`true / false`, `1 / 0` or `✓ / ✗`), whether binary values are hex, Base64 or UTF-8 text, and whether JSON in a cell is shown compact or pretty. Turn on **Use these formats for export** to have exported files follow them.

### Driver properties

Under Drivers you can pass extra properties to a driver for every connection that uses it. For PostgreSQL that includes `application_name`, `statement_timeout` and `search_path`. For MySQL, `sql_mode`, `time_zone` and `wait_timeout`. For SQLite, `foreign_keys`, `journal_mode`, `busy_timeout` and `cache_size`.

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

### Editing

| Action | Shortcut |
|---|---|
| Find, find next, find previous | ⌘F, ⌘G, ⇧⌘G |
| Find and replace | ⌥⌘F |
| Toggle line comment | ⌘/ |
| Duplicate line or selection | ⌘D |
| Format SQL | ⌥⌘L |
| Complete | Ctrl+Space |
| Undo, redo | ⌘Z, ⇧⌘Z or ⌘Y |
| Indent, outdent | Tab, ⇧Tab |

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
| Installed drivers | `drivers/<id>/<version>/` in the data directory |
| Passwords | your OS keychain, under the service name `sqlr` |
| History and snippets | `sqlr.db` in the data directory |
| Logs | `logs/sqlr.log.<date>` in the data directory |

On macOS, both directories are inside `~/Library/Application Support/dev.sqlr.sqlr/`. On Linux, the config directory is `~/.config/sqlr/` and the data directory is `~/.local/share/sqlr/`. Use `--config-dir` and `--data-dir` to put them somewhere else.

The log level follows the `RUST_LOG` environment variable and defaults to `info`. Passwords are kept out of logs.

## Performance you can count on

sqlr is built to stay light, and the numbers below are checked on every change:

| What | Target |
|---|---|
| Start to a usable window | under 1 second |
| Memory while idle with a couple of connections open | under 150 MB |
| Scrolling a grid with over a million rows | 60 frames per second |
| Autocomplete popup, opening a table | under 50 ms |
| Size of the application | under 50 MB |

Details are in [Performance budgets](../performance-budgets.md).
