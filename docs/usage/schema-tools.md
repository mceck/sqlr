# Schema tools

Four tools help you understand a schema and carry changes from one place to another.

## Compare schemas

Use **Compare schemas** from the palette (⌘⇧P) to see how two schemas or databases differ. They can be on the same connection or on two connections of the same kind of database, which is the usual case for comparing development with production.

You get:

- a tree of the differences, grouped by object, so you can see at a glance what was added, removed or changed
- a side-by-side view of the DDL of any object that differs
- a **migration script** that turns the first schema into the second. The script puts its statements in an order that works (a table before the foreign key that points to it, for instance) and opens in a normal editor tab, so you can read it, adjust it and run it yourself

You can leave individual objects out of the comparison and the script.

The script is a starting point for you to review, and it is only ever run when you run it.

## ER diagrams

Open a diagram for a whole schema or a selection of tables. sqlr lays the tables out using their foreign keys, so related tables end up near each other. Drag tables to arrange them as you like. The layout is saved for each schema, so the diagram looks the same next time.

Export the diagram as **SVG** for documentation or slides. The SVG is exact and opens in any browser or vector editor.

## Query plans

Choose **Explain** (⌥⌘E) to see how the database intends to run the statement, or **Explain Analyze** (⌥⇧⌘E) to run it and see what really happened. The **Explain** button in the strip above the results does the same, with the chevron beside it for Analyze. The plan opens in a **Plan** pane and stays there until you recompute it, even if you switch back to the result.

The pane starts with a summary: execution and planning time, cost, rows, the mix of operators and, when something looks wrong, warnings in plain words. A row estimate off by 10× or more, a full scan over a large table, a node run thousands of times and a sort that spills to disk are called out by name. Below it, the hot path bar splits the time (or the cost, without Analyze) between nodes: hover a segment to see it and click it to select its node. The operators themselves are cards connected as a tree; each shows its estimated and actual rows, cost and share of the time, and the heaviest ones are marked **HOT**. Select a card to read its filters, indexes, loops, buffers and other properties in the inspector on the right. **Collapse all** folds long plans, **Raw** shows the server's own plan, and **Copy raw plan** takes it with you.

Explain Analyze runs the statement for real. For a statement that changes data the app asks first and runs it inside a transaction that is rolled back; triggers and other side effects still happen. A running explain can be stopped, and the statement is explained on the tab's own session, so temporary tables and session settings apply.

## Generate test data

Right-click a table in the explorer and choose **Import/Export → Generate data**. For each column you pick a generator: sequences, random ranges, names, emails, dates, or values picked from the rows that a foreign key points to, so the data respects your relations.

You set:

- the number of rows
- a **seed**. The same seed gives the same rows, which is useful when a test needs reproducible data
- how often a nullable column is left NULL
- columns you want the database to fill itself (a serial id, for example), by unchecking them

**Preview** shows sample rows before anything is written. **Generate** writes them through the same engine as data transfer, in batches, and **Save as task** keeps the setup for later.
