# Results and data editing

## Reading results

Results appear in a grid under the editor. The grid draws only the rows and columns you can see, so a result with a million rows scrolls as smoothly as one with ten.

### Loading more rows

sqlr fetches results in pages, 500 rows by default. Scroll near the bottom and the next page loads. The status bar shows **Fetch: 500**, which you can change per result (100, 500, 1000, 5000 or a number of your own). **Fetch all** streams the rest of the result with visible progress, and you can cancel it at any time. The global default, and the cap for Fetch all, are in Settings under Result grid.

Large results do not have to fit in memory. When a result grows past its memory budget, older parts are moved to a temporary file and read back when you scroll to them. Closing the result tab releases everything.

### Sorting and filtering

Click a column header to sort. Use the filter field above the grid to narrow the rows. When the whole result is already loaded, sorting and filtering happen instantly on your machine. When only part of it is loaded, sqlr offers to run the query again on the server with the sort or filter applied, so you see the right rows and not just the right order of the ones you happened to fetch.

### Reading values

- NULL is shown in grey italics, so it is never confused with the text "NULL" or an empty string.
- Numbers are right-aligned. Exact decimals stay exact.
- Dates, times, timestamps, UUIDs, JSON, XML, arrays, binary and geometry values each get a suitable rendering.
- Long values are cut in the grid. Select the cell and the **value panel** on the right shows the whole thing: formatted JSON, hex for binary, a preview if the bytes are an image. Toggle the panel from the View menu.

How dates, numbers, booleans, binary data and JSON look is up to you. See [Settings](settings-and-shortcuts.md).

### Selecting and copying

Click and drag to select a rectangle of cells, and move around with the arrow keys as in a spreadsheet. Hold **⌘** (Ctrl on Windows and Linux) and click cells or row numbers to add separate, non-contiguous ranges; click one again to remove it. You can freeze columns so they stay in view while you scroll sideways.

**Copy** puts the selection on the clipboard as tab-separated text. For a non-contiguous selection it keeps the selected rows and columns, with empty fields where a cell was not selected. **Copy as** offers CSV, JSON, SQL `INSERT` statements and Markdown, which is handy for pasting into a chat, a ticket or a README.

**Paste** (⌘V) writes tab-separated clipboard text back into an editable result. A single value fills every selected cell; a block of cells starts at the top-left corner of the selection.

To save a whole result to a file, use **Export result** (see [Import, export, transfer and backup](moving-data.md)).

## Editing data

When a result comes from a single table that has a primary key or a unique key, you can edit it right in the grid.

- **Double-click** a cell to change it.
- Use **Add row**, **Duplicate** and **Delete** for whole rows.
- **⌘⌫** (⌘Delete) marks the selected rows deleted.
- Edited cells, new rows and deleted rows each get their own tint, so you can see at a glance what is pending.

Nothing is sent to the database yet. Press **Preview** to read the `INSERT`, `UPDATE` and `DELETE` statements sqlr generated, then **Apply** to run them, or **Discard** to drop the changes.

Apply runs the statements in a transaction. If one of them fails, the transaction is rolled back and nothing changes. If someone else changed a row you were editing in the meantime, sqlr notices and names the conflicting rows instead of silently overwriting their work.

### Which results are editable

sqlr finds a row by its primary key or a unique key, and only edits rows it can identify exactly. A result is editable when:

- it comes from one table, and
- the table has a primary key or a unique key, and
- the connection is not in read-only mode.

When a result is read-only, the grid tells you why. A result from a join, an aggregate or a table without a key shows the reason, so you know what to change (for instance, add a primary key to the table).

Documents in MongoDB and Elasticsearch are identified by `_id`. Redis values are edited per key or per field. See [Document and key-value databases](nosql.md).

## Seeing the query's story

The **Messages** tab next to the results lists, for each statement of a run, what it was, how long it took, how many rows it touched, and any notices or truncation the server reported. When a long script misbehaves, this is the first place to look.
