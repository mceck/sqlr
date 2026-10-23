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

Choose **Explain** (⌥⌘E) to see how the database intends to run the statement, or **Explain Analyze** (⌥⇧⌘E) to run it and see what really happened. The plan appears as a tree, one node per step, with the cost, the number of rows and the time of each. In Explain Analyze, badges compare the estimated rows with the actual ones, which is often where a slow query gives itself away. The most expensive nodes are highlighted, and you can copy the raw plan.

Explain Analyze runs the statement for real. For a statement that changes data, run it inside a transaction you can roll back.

## Generate test data

Right-click a table in the explorer and choose **Generate data**. For each column you pick a generator: sequences, random ranges, names, emails, dates, or values picked from the rows that a foreign key points to, so the data respects your relations.

You set:

- the number of rows
- a **seed**. The same seed gives the same rows, which is useful when a test needs reproducible data
- how often a nullable column is left NULL
- columns you want the database to fill itself (a serial id, for example), by unchecking them

**Preview** shows sample rows before anything is written. **Generate** writes them through the same engine as data transfer, in batches, and **Save as task** keeps the setup for later.
