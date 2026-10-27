---
status: done
---

# Settings and workspace

## Specs

- [Settings and workspace](../specs/settings-and-workspace.md)
- [Connections](../specs/connections.md)
- [Query execution](../specs/query-execution.md)
- [Explorer and objects](../specs/explorer-and-objects.md)
- [Agents and terminal](../specs/agents-and-terminal.md)
- [Drivers](../specs/drivers.md)
- [Diagnostics](../specs/diagnostics.md)

## Problem Statement

A database client is opened dozens of times a day, and every detail of how it opens decides whether it feels fast or tiring: where the panels are, whether the last Connection is one keystroke away, whether the status bar answers "is a transaction open", whether the shortcut users already know works. Different users work differently, and the same user needs different behaviour on a local database and on production.

A single global configuration cannot express that: a shortcut set for the editor should not apply while the Explorer has focus, a safety rule for production must survive a global preference or an agent, and a theme choice must not silently change the text on a custom accent to something unreadable. Then the app has to start, update and install on three platforms without ceremony, because none of this is what the user came to do.

## Solution

The main window is a fixed, fast shell: a 44-pixel toolbar with the Connection chip and run controls, a Connections panel, a dockable tab area, optional Operations, history, value panel and terminal, and a 26-pixel status bar that always shows the Connection, the transaction and the caret.

The welcome screen fills the empty window with recent Connections, database tiles and quick starts. The Palette opens any object across every Connection or any command from the keyboard. Preferences has a searchable section for every area of behaviour and a scope picker that sets a value globally, per Connection profile or per Connection, with Safety flags that only tighten.

Every command is an Action with a group and a scope, bound by a remappable Keymap with the sqlr and VS Code presets and conflict detection. Themes, a UI scale, a custom accent and a bundled font with lazy fallbacks keep the interface consistent. The binary takes directories, version and help arguments. A daily, silent, checksum-verified update check installs on macOS and Linux and points Windows at the release. A tagged build produces the platform packages and checksums.

## User Stories

1. As a user, I want a consistent toolbar with the Connection I am on and the controls I use, so that I always know where I am.
2. As a user with many Connections, I want the connection chip to switch, connect, edit, reconnect and disconnect without opening a dialog, so that changing target is quick.
3. As a user, I want Run, Run script and Stop together, so that execution is one glance away.
4. As a user in a transaction, I want commit and rollback in the toolbar, so that finishing a transaction is immediate.
5. As a user on a narrow window, I want the toolbar to fold its menus and secondary controls instead of overflowing, so that the window stays usable.
6. As a user, I want a colored Connection chip and a breadcrumb with the database, so that production is never a guess.
7. As a user, I want the database picker blocked while a query runs or a transaction is open, so that I cannot change database mid-flight by accident.
8. As a user, I want toasts in the corner for short-lived results and errors, so that feedback does not interrupt.
9. As a user, I want an error toast to stay while I read it, so that it does not vanish before I act.
10. As a user, I want the status bar to show the Connection state, the transaction, the rows and time, the selection, the fetch size and the caret position, so that I never have to open another panel for the basics.
11. As a user on a narrow window, I want the status bar to drop the least important segments first, so that the important ones stay readable.
12. As a user, I want the Operations panel to list running work with a cancel action and to clear finished work after a while, so that I can see and stop background work.
13. As a user, I want the panels I open to keep their size, so that I can arrange the window around my task.
14. As a user, I want tabs I can reorder and split side by side, so that I can compare two things at once.
15. As a new user, I want a welcome screen with the databases sqlr knows and a demo, so that I can start without knowing what to type.
16. As a returning user, I want my recent Connections first with how long ago I used them, so that I resume in one click.
17. As a user arriving from another client, I want a quick start that imports my saved Connections, so that moving is cheap.
18. As a keyboard user, I want the Palette to find any table across every Connection, so that I do not navigate trees.
19. As a keyboard user, I want the Palette to run any command, find a snippet or reopen a history entry, so that the mouse is optional.
20. As a user, I want the Palette to show each result's shortcut, so that I learn the key as I use it.
21. As a user, I want Preferences to search every setting by its words, so that I find a setting whose section I do not remember.
22. As a user, I want to set a value globally, per profile or per Connection, so that one environment can differ without reconfiguring everything.
23. As a user, I want to see which level a value comes from and reset an override, so that I understand where it is set.
24. As a user, I want global-only settings locked at lower levels, so that I cannot create a nonsensical per-Connection theme.
25. As a user, I want Apply to keep the window open and Save to close it, so that I choose how I finish.
26. As a user, I want Cancel and closing the window to restore the values I opened with, so that a mistake costs nothing.
27. As a user, I want a Safety flag switched on anywhere to apply everywhere below, so that protection cannot be undone by accident.
28. As an administrator, I want the settings files to ignore a false Safety flag, so that a hand-edited file cannot loosen protection.
29. As a user, I want every command remappable with the sqlr or VS Code preset as a start, so that my muscle memory carries over.
30. As a user, I want shortcuts to be scoped to the editor, the Explorer or the whole window, so that one chord can mean different things in different places.
31. As a user recording a shortcut, I want to see conflicts before saving, so that two commands never fight.
32. As a user, I want Escape to cancel recording and Enter to keep working, so that recording does not trap the keyboard.
33. As a user, I want light, dark and system themes with a custom accent, so that the app fits my environment.
34. As a user, I want the whole interface to scale, so that it is comfortable on a large or small display.
35. As a user, I want the editor font size configurable, so that long SQL is readable.
36. As a user typing CJK or emoji, I want those glyphs to render without loading every fallback font at startup, so that the app starts fast.
37. As a user with several installations, I want to point sqlr at another config or data folder, so that I keep them separate.
38. As a user, I want `--version` and `--help` to answer without opening a window, so that scripts and diagnostics work.
39. As a user, I want the app to tell me when a newer release exists and install it only if I say so, so that updates are my choice.
40. As a user, I want the update download verified against the release checksums, so that a corrupted download is never installed.
41. As a user without a network, I want a failed update check to be silent, so that startup is never blocked by it.
42. As a user, I want to check for updates on demand and see a skipped version, so that I can change my mind.
43. As a user, I want to restart into the new version from the app, so that installing is complete.
44. As a user, I want to skip a version, so that it is not offered again automatically.
45. As a user on macOS, Linux or Windows, I want a release build for my platform and architecture, so that installing does not need a toolchain.
46. As a user, I want a checksum file with every release, so that I can verify a manual download.
47. As a user, I want the download page to pick the right file for my platform, so that I do not choose the wrong one.
48. As a maintainer, I want a tag that does not match the app version to fail the release, so that a mislabelled build cannot ship.
49. As a maintainer, I want CI to check formatting, lints, dependency rules and tests, so that main stays releasable.
50. As a privacy-conscious user, I want no telemetry and a switch for the update check, so that the only network traffic is mine.

## Implementation Decisions

- A core settings catalogue declares every setting once: identifier, section, label, description, keywords, type, default, accepted levels, Safety flag, and agent readability and writability. The Preferences window, settings search, defaults and the Agent interface read it. A test enforces that every section name is known, every default is valid and every Safety flag defaults to off with all levels.
- Layered settings resolve from the most specific level to the default. The store reads and writes a versioned global settings file and a separate overrides file grouped by profile and Connection, migrates the older flat layout once with a backup, and moves corrupt files aside.
- Safety flags are resolved as a logical OR across levels; the overrides reader drops a false value for a Safety flag and drops global-only keys, so a lower level or an edited file can only add protection.
- The Preferences window works on a copy of the settings and a working Keymap, with a per-section dirty state and a scope picker built from the catalogue's accepted levels and the known profiles and Connections. Apply and Save hand the copy to a background saver that serialises saves so an older state cannot land last; Cancel discards the copy.
- The Keymap is a list of Action identifier and chord pairs, stored as a named preset plus the differences. The core parses, normalizes and labels chords, provides the sqlr and VS Code presets, and reports a conflict only when two actions share a chord and their scopes overlap. The Keymap editor is a widget over that model with a recording state.
- Actions are dispatched according to scope: global actions from the app's key handling, editor actions while an Editor tab has focus, Explorer actions in the Explorer. The toolbar, menus and native menu map the same Action identifiers, and the native menu builds its accelerators from the Keymap.
- The Palette is split: the app builds the item list from the Catalogs, snippets and history whenever they change, and the interface layer ranks with fuzzy matching, normalizes accents, caps the list and draws the window.
- The theme is a token palette selected from the theme choice, with the accent corrected for contrast, applied to the toolkit each frame. The spacing scale sets the toolbar, tab and status heights. Fonts are installed once from bundled faces, with lazy platform CJK fallbacks loaded on a worker.
- The welcome screen is a widget over cards, recent Connections and quick-start actions the app produces; relative times are computed without reading the local time zone on the UI thread.
- The command line is parsed by hand into Run with optional data and config directories, Version, Help or a usage error. Errors print the usage and exit with code 2.
- Updates are a background state machine with a state file, a strict host and redirect policy, timeouts, size caps, checksum verification, and platform install steps. The automatic check runs at most daily, silently, while an explicit check reports its outcome. The install relaunches on request.
- The release workflow is driven by a version tag, builds each platform and architecture, packages the application and the Sidecar drivers, writes the checksum file and publishes the release. Continuous integration runs formatting, Clippy, the dependency check and the test suite.

## Testing Decisions

- Core catalogue seam: one invariant test over every setting (unique identifiers, known sections, valid defaults, JSON round trip, Safety flag rules, agent visibility, global-only level rules), plus search matching. Prior art: the settings model tests.
- Core Keymap seam: chord parsing and labelling per platform, preset contents, storage as a difference, conflict detection with overlapping and disjoint scopes, and corrupt and missing files. Prior art: the Keymap module tests.
- Store seam: layered resolution across global, profile and Connection, Safety flag OR and false-value dropping, global-only refusal, legacy migration with backup, corrupt file handling, and Keymap storage. Prior art: the layered settings and Keymap store tests.
- App seam: `egui_kittest` and app-level tests for the Preferences window (search, scope picker, override dot and reset, locked global-only rows, dirty state, Apply, Save, Cancel, conflict block, open settings folder), the Keymap editor (record, reset, remove, preset switch, conflict banner, Escape), the Palette (groups, shortcuts, ranking, cap, keyboard), the welcome screen (new and returning variants, relative times, tiles, demo), the toolbar and status bar (segments, priority hiding, transaction line, fetch combo), the Operations panel, toasts, and the command line. Prior art: the existing prefs, palette, welcome, shell and CLI tests.
- Update seam: the state machine with a fake HTTP layer covering due checks, silent auto failure, explicit failure, skipped versions, host and redirect refusal, size caps, checksum failure, install and relaunch, and the state file. Prior art: the updates module tests.
- Theme seam: palette selection for light, dark and system, accent contrast correction, and token values. Prior art: the theme tests.

## Acceptance Criteria

- [x] The toolbar is 44 pixels, tabs 34 and the status bar 26, and the Connections panel defaults to 20% clamped between 200 and 270 with a 160 minimum.
- [x] The connection chip shows the Connection's color and state and offers switch, connect, edit, reconnect and disconnect.
- [x] The toolbar has Run, Run script and Stop, transaction controls inline or under More tools, and the right-hand toggles.
- [x] The toolbar folds the classic menus into a Menu button when narrow and hides the in-window menus on macOS.
- [x] The breadcrumb shows Connection, database and tab, disables the database picker during a run or transaction, and shows the run shortcut.
- [x] Toasts cap at three, use 4 seconds for information and success and 8 for errors, fade over 0.6 seconds, and stay while hovered.
- [x] The status bar shows the Connection state, the transaction line, rows and time, selection, fetch size presets and custom value, Load all, running export progress, MCP status, encoding, line and column, and the not-saved marker, dropping segments by priority.
- [x] The Operations panel is 200 pixels, follows the retention setting, lists operation state and offers individual cancel, cancel all with confirmation, clear completed and close.
- [x] Tabs can be reordered and split in the central dock area, and closing the last tab shows the welcome screen.
- [x] The welcome screen shows the eight database tiles with install state, up to eight recent Connections with relative times, New connection, the demo and import quick starts, and More databases.
- [x] A new user sees Create a connection first; a returning user sees Recent connections first.
- [x] The Palette opens with `⌘P` and `⇧⌘P`, searches objects across all Connections, commands, snippets and history, ranks fuzzily with accent normalization, caps at 50 rows, and shows groups, icons and shortcuts.
- [x] Preferences is 880 pixels wide with 10 sections, a search over identifiers, labels, descriptions and keywords, per-section dirty dots and a 196-pixel section list.
- [x] Preferences has General, Appearance, Editor, Result grid, Data formats, Keymap, Connections, Agents, Terminal and Drivers, with the documented settings and defaults.
- [x] Apply saves and keeps the window open; Save saves and closes; Cancel and closing restore the original values; the settings save runs on a background thread one at a time.
- [x] A scope picker sets values at global, profile or Connection level, shows an override dot and a reset, shows where a value comes from, and locks global-only settings.
- [x] Safety flags resolve as on when any level has them on, and a false value at a lower level cannot turn one off; the overrides reader drops such values.
- [x] A legacy flat settings file is migrated with a backup, and a corrupt settings or Keymap file is preserved and defaults are used.
- [x] Every action is remappable with the sqlr or VS Code preset, stored as a difference from the preset, and conflicts are reported only when scopes overlap, blocking save.
- [x] The Keymap editor records a chord, cancels on Escape and not on Enter, removes and resets bindings, switches presets, and searches actions and shortcuts.
- [x] Light, dark and system themes, UI scale 0.75 to 2.0 and a custom accent with contrast correction apply to the whole interface.
- [x] The interface uses bundled Inter and Phosphor, and the editor and grid use the interface toolkit's monospace at the configured size.
- [x] CJK glyphs load a platform fallback font lazily on a worker, and the toolkit's emoji fallback stays in the chain.
- [x] `--data-dir`, `--config-dir`, `--version` and `--help` work, both option forms are accepted, and a usage error exits with code 2.
- [x] Open settings folder opens the configuration folder with the file manager.
- [x] The update check runs at most daily and silently by default, reports on demand, and remembers the skipped version and last check time.
- [x] The update client restricts hosts and redirects, enforces timeouts and size caps, verifies the release checksum, and refuses a release without a checksum file.
- [x] Install works on macOS and on a writable Linux folder, opens the release page on Windows, and offers Restart now after installing.
- [x] A release tag must match the app version, and the workflow publishes platform packages, Sidecar drivers and `SHA256SUMS.txt`.
- [x] CI checks formatting, Clippy with warnings as errors, dependency rules and the test suite.
- [ ] The window layout, panels and tab groups are saved per window and restored on the next launch.
- [ ] The monospace font family is configurable, not only its size.
- [ ] Native client tool paths are configured in a Settings Tools section.
- [ ] Result grid colors are configurable from Preferences.
- [ ] macOS releases are signed and notarized and offered through a Homebrew cask.
- [ ] Windows releases are offered as an MSI and through winget.
- [ ] Linux releases are offered as a deb package and a Flatpak.

## Out of Scope

- Syncing settings between machines or through an account.
- Importing keymaps from other clients.
- Per-window or per-workspace profiles.
- A visual theme editor.
- A beta or nightly release channel.
- Auto-updating without the user choosing Install.

## Further Notes

This task records retroactively the area as it exists. The unticked criteria come from the earlier design document and the wider product plan: the window layout and panel sizes start from the default arrangement on each launch; only the monospace size is configurable; native tool paths live in the Dump and Restore dialogs and in each Driver's properties rather than in a Settings Tools section; the grid has no color settings; and the release builds a dmg, a Linux tarball and AppImage, and a Windows zip, without notarization, a cask, winget, a deb or a Flatpak.
