---
name: sqlr-configure
description: Change sqlr's settings, Connection profiles, keyboard shortcuts and connection folders through the sqlr MCP tools. Use when the user asks to change how sqlr looks or behaves (theme, fonts, page size, date formats, timeouts, confirmations, read-only, commit mode), to create or edit a profile, to rebind a shortcut, or to organise connections into folders.
metadata:
  sqlr-version: 0.1.4
---

# Configure sqlr

sqlr's configuration is read with `config_list` and changed with `config_write`; folders, profiles and Connections are removed with `config_delete`, which always waits for the user's Approval. Every change applies to the running app at once and is saved by sqlr: never edit sqlr's files on disk.

If the sqlr tools are missing or failing, use the `sqlr-connect-agent` skill.

## Read before you write

Call `config_list` with the target you need (`settings`, `profiles`, `keymap`, `folders`, `connections`) and work from what it returns:

- Each setting carries its id, label, description, type, default, `allowed_levels`, `safety` and the values stored at each level. Use the description to confirm you picked the right one.
- `agent_writable: false` means the setting is the user's alone. Tell the user where to change it in Settings instead of trying.

## Choose the level

A setting can be stored at three levels; a lower level overrides a higher one: **global**, then **profile** (every Connection using that Connection profile), then **Connection**.

- App-wide options (appearance, editor, formatter, terminal, memory budgets, update check) accept only `global`.
- "For this connection" → the Connection level.
- "For all my production databases" → the profile those Connections use (usually Production). Check with `config_list` (`connections`) which profile each Connection has.
- "Everywhere" → global.
- When the request does not say, and the setting accepts several levels, ask the user before writing.

Resetting a level removes its override so the level above shows through.

## Safety flags only tighten

`safety.read_only`, `safety.confirm_writes` and `safety.confirm_destructive` are Safety flags. Through the agent interface they can be switched on, never off, at any level. The same holds for profiles:

- a profile's `read_only`, `confirm_writes` and `confirm_destructive` that are on stay on;
- `idle_tx_timeout_min` can only decrease;
- `commit_mode` can only get stricter: `auto` → `smart` → `manual`;
- a Connection can move only to a profile at least as strict;
- driver fields that weaken transport security (certificate and host-key verification) can only be tightened.

A rejected change answers `invalid` naming the flag. Do not look for another route to the same relaxation; tell the user they can change it in sqlr.

## Common requests

| The user wants | Setting |
| --- | --- |
| Light or dark theme, accent colour | `appearance.theme`, `appearance.accent` |
| Bigger or smaller UI | `appearance.ui_scale` |
| Editor font size, line numbers, soft wrap | `editor.font_size`, `editor.line_numbers`, `editor.soft_wrap` |
| Autocomplete behaviour | `editor.completion_auto_open`, `editor.completion_delay_ms`, `editor.completion_keyword_case` |
| Formatter style | `formatter.keyword_case`, `formatter.indent`, `formatter.max_line`, `formatter.comma_first` |
| More rows per page, a higher row cap | `grid.page_size`, `grid.row_cap` |
| How NULL is shown | `grid.null_text` |
| Date, time and number formats | `formats.date`, `formats.timestamp`, `formats.number_grouping`, `formats.decimals` |
| Stop a script at the first error | `general.stop_script_on_error` |
| One session per editor tab | `connection.session_per_tab` |
| Connect timeout | `connection.connect_timeout_s` |
| Ask before writes or destructive statements | `safety.confirm_writes`, `safety.confirm_destructive` (on the profile or Connection) |
| Make a database read-only | `safety.read_only` (on the profile or Connection) |
| PostgreSQL `search_path`, statement timeout | `drivers.postgres.search_path`, `drivers.postgres.statement_timeout` |
| MySQL SQL mode or time zone | `drivers.mysql.sql_mode`, `drivers.mysql.time_zone` |
| Terminal font size or scrollback | `terminal.font_size`, `terminal.scrollback` |

For anything else, search the `config_list` settings by label and description.

## When a change takes effect

- Appearance, editor, grid, formats and `terminal.font_size` apply immediately.
- `terminal.cwd` and `terminal.scrollback` apply when the user restarts the terminal.
- `drivers.*` and `connection.*` settings apply to the next Session that opens; tell the user to reconnect when they want them now.

Tell the user which of these applies after each change.

## Profiles

A Connection profile sets commit mode, read-only, write and destructive confirmations, idle transaction timeout and colour for every Connection using it. The built-in profiles are Development, Test and Production. To create one, read the closest existing profile with `config_list` (`profiles`), then `config_write` a new profile based on it. A profile still used by a Connection cannot be deleted.

## Keymap

`config_list` (`keymap`) lists every action with its chords and whether it differs from the preset. `config_write` (`keymap`) sets the chords of one action or resets it to the preset. Check the new chord is not already bound to another action first.

## Folders and moving Connections

Folders are created, renamed and moved with `config_write` (`folder`); a Connection moves by patching its folder with `config_write` (`connection`), which works for Connections the user exposed. Deleting a folder moves its Connections and subfolders to the parent, after an Approval.

## Left to the user

The MCP server switch, its port and token, the terminal shell command, driver installs, app updates, Agent access on Connections, and the privacy settings (update check, SQL in diagnostics). Point the user to Settings when they ask for these.
