# Grid editing and clipboard operations

## Agreed behavior

- Repeat a pasted block in every selected rectangle, tiling when both dimensions are
  exact multiples. Otherwise paste once from that rectangle's top-left corner, retaining
  the existing clipping at the grid boundaries. A single cell remains a paste anchor.
- Preserve scalar fill across arbitrary selected cells.
- Validate every affected value before committing an operation to the pending edit buffer.
- Set NULL across selected cells and duplicate every selected row once.
- Revert only selected cells without dropping other changes in their rows.
- Undo and redo whole operations before Apply. A new edit clears redo; Apply or reloading
  a result starts a new history. These actions never undo committed database writes.
- Keep typed clipboard data within the application, including column names, NULL versus
  empty text, binary values, and unselected holes. Plain text remains available to other apps.
- Paste as new rows through a mapping preview: match column names, manually map or skip
  unmatched columns, validate required target columns, and regenerate automatic IDs (including defaulted primary keys) by
  default. Preserve explicit IDs only where the target supports it.
- Copy all results runs the current read query again, including its SQL filters and limits,
  as a cancellable background job with progress, rather than copying only loaded rows.

## Implementation choices

- Keep the clipboard model in core, grid editing and mapping UI in ui, and background
  clipboard jobs in engine. The app connects jobs to the operating-system clipboard.
- Keep all grid changes pending until Apply and reuse existing database transactions.
- Copy all reads database values; require pending edits to be applied or discarded first.
- Bound clipboard memory and report when export should be used instead.
- No cross-database query changes, structural table copying, fill-down/fill-right commands,
  or selection preservation across sorting are included.

## Verification

UI interaction tests cover repeat/fallback paste, multiple selections, transactional
validation, undo/redo, selective revert, typed clipboard round trips, row duplication,
and mapping validation. Engine tests cover complete reads, read-query enforcement,
progress and cancellation. Run formatting and checks for affected crates, then fast
workspace checks where practical.
