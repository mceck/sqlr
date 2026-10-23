# The explorer and object editors

## The explorer

The explorer is a tree of everything you can reach. Under a relational connection you find databases, schemas, tables, views, indexes, sequences, functions and triggers. Under MongoDB you find databases and collections, under Elasticsearch indices, under Redis the numbered databases and their keys.

Children load when you expand a node, so a server with thousands of tables opens instantly. Folder rows show how many objects they hold. A small text field at the top filters the tree as you type.

On startup the explorer and autocomplete answer from a saved copy of the last schema while a refresh runs in the background, so you can start working before the server has replied. The saved copy is renewed after any DDL statement you run on that connection, and whenever you refresh by hand.

### Context menu

Right-click an object for the actions that apply to it. Depending on the object and the database you will see:

- **Open data** to browse a table or view
- **New console** to open a query tab on that connection
- **Open DDL** to read the `CREATE` statement in a read-only tab, and **Copy DDL** to copy it
- **Import/Export** for **Export data**, **Import data**, **Generate data** and **Transfer data**
- **Generate SQL** for `SELECT`, `INSERT`, `UPDATE`, `DELETE` and `CREATE` templates
- **Copy name**
- **Rename** (or press F2), **Duplicate**, **Truncate**, **Drop**
- **Refresh**, **Edit**, **Disconnect**, **Remove** on connections

Destructive actions such as Drop and Truncate go through your profile's confirmation rules.

## Object editors

Open an object for editing (double-click it, or right-click and choose **Edit**) and it opens in a tab with sub-tabs. A table, for example, has Columns, Indexes, Constraints, Foreign keys, Triggers, Permissions, DDL, Data and Properties. What you see depends on what the database supports for that kind of object.

You can edit tables, views, materialized views, indexes, sequences, functions and procedures, triggers, schemas and databases. To create one, use the **New ...** entries in the explorer menu or in the command palette.

### How changes are applied

Edits in an object editor do not touch the database right away. They collect in a list of pending changes, and the tab tells you how many there are. Then:

1. Press **Preview DDL** to read the exact statements sqlr will run.
2. Press **Apply** (⌘S) to run them. Where the database supports transactional DDL, the whole batch runs in one transaction, so it either all happens or none of it does. Where it does not, statements run one by one and sqlr tells you which ones went through if one fails.
3. Or press **Discard** to forget the changes.

This makes it easy to rename three columns, add an index and change a default in one go, look at the result, and only then commit to it.

### Reading DDL

**Open DDL** shows an object's definition in an editor tab. sqlr asks the server for it where the server can produce it (`SHOW CREATE` in MySQL, `pg_get_*def` in PostgreSQL, `sqlite_schema` in SQLite), so what you read is what the database really holds.

## Opening objects quickly

⌘P opens a search over all objects of all your connections. Type part of a name and press Enter to open it. It works from the saved schema copy, so it answers immediately.
