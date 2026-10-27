[Manual](README.md) › Everyday work

# Read and edit query results

The result grid shows fetched rows beneath your editor. You can inspect and copy any readable result; editing requires a writable session, a capable driver and identifiable rows from one table.

## Load the rows you need

sqlr fetches 500 rows at a time by default. Scrolling near the end loads another page while more rows remain. Change **Fetch:** for the next page, or choose **Fetch all** to continue until the result ends or reaches its limit.

| Default limit | What happens when reached |
| --- | --- |
| 100,000 rows for **Fetch all** | The stream closes and the result reports truncation |
| 128 MiB per SQL result | Older chunks move to temporary storage and loading continues |
| 512 MiB for all SQL results | Older results spill to temporary storage so fetching can continue |

Adjust these in **Result grid** preferences. Memory limits apply to SQL row buffers. Older chunks are stored in private temporary files and remain readable as you scroll, copy, sort or edit. The operating system manages mapped file pages, so these limits are not an exact ceiling on the app's total RAM. A storage failure stops the fetch with an error and keeps previously loaded rows. Closing a tab or rerunning releases its old results; temporary storage is reclaimed after the last reader finishes. A later query on the same session can also cut short a result whose rows are still on the server.

## Resize columns

Drag the right boundary of a column header to change its width. Double-click the boundary to fit the column to its header and its loaded values, as in a spreadsheet. Resizing does not change sorting or selection.

## Sort, filter and inspect values

Click a header to sort a complete result locally. If rows remain unfetched, sqlr offers **Sort on server**, which reruns the query with ordering. The SQL filter condition always runs on the server, including when all rows are loaded. **Filter by this value** builds a condition for you. Applying a sort or filter that reruns the query replaces the result; apply or discard edits first.

NULL has a distinct muted rendering. Long values are shortened in the grid. Select a cell and use the value panel to see its full text, formatted JSON, binary representation or supported image preview. Configure date, numeric and other display formats in [Settings](settings-and-shortcuts.md).

## Select and copy

Drag a rectangle, click row numbers or headers, and use arrow keys to move the selection. Shift extends a range; ⌘-click adds separate ranges.

**Copy** uses tab-separated text. **Copy as** offers TSV, CSV, JSON, SQL `INSERT` and Markdown. Copying between sqlr grids also preserves types, binary values and the distinction between NULL and empty text while the clipboard text remains unchanged.

**Copy all results** reruns the read query, including its SQL filters and limits, to copy rows beyond those loaded. It is unavailable with pending edits. Large copies run as cancellable operations after confirmation. Clipboard content is capped at 128 MiB; use [export](moving-data.md) for larger datasets.

## Make pending edits

1. Open a table's data, or run a query returning its columns and row key.
2. Double-click a cell, or select it and press Enter.
3. Enter a valid value. Long text, JSON and binary values use the value panel's editor.
4. Use **Add row**, **Duplicate row(s)** or **Delete row(s)** for whole rows.
5. Choose **Preview SQL** to inspect generated statements.
6. Choose **Apply** to execute the changes, or **Discard** to abandon them.

Edits are tinted; new and deleted rows have distinct colours. **Set NULL** requires nullable writable cells. **Revert selected cells** and **Revert row(s)** remove pending changes. Undo and redo work before applying, including for a whole paste; they do not reverse committed database writes.

A result needs column origins from one table and every column of its primary key or a suitable non-null unique key. Joins, computed columns, missing keys, unsupported drivers and read-only sessions can prevent editing. The result bar explains the reason and can offer **Open table to edit**.

## Paste values or insert rows

⌘V pastes TSV text or sqlr's typed clipboard into selected cells. One value fills the selection. A block repeats when the target dimensions are exact multiples; otherwise it starts at each selected area's top-left corner and clips at the grid boundaries. An invalid value rejects the entire paste.

**Paste as new rows…** opens a column-mapping preview. Map source columns, choose **Skip** where needed, and supply required destination columns. Automatic IDs regenerate by default. **Add pending rows** adds the validated rows to the changeset; you still need **Apply**.

## Understand Apply

Apply is atomic for the changeset: a failure rolls back its statements and keeps edits available. In **Auto**, sqlr commits its own transaction. In **Manual** or **Smart**, it leaves the transaction open for your decision. Within an existing transaction, a savepoint protects earlier work.

Updates and deletes must each match exactly one row by their original key. A different affected-row count rolls back the changeset and asks you to refresh. This detects missing or changed keys; it does not compare every old cell value against concurrent edits that retain the same key.

## Related

- [Profiles and transactions](profiles-and-transactions.md): finish an applied transaction.
- [Document and key-value databases](nosql.md): edit documents and Redis values.

---

← [SQL editor](sql-editor.md) · [Profiles and transactions](profiles-and-transactions.md) →
