# The Mongo shell subset

sqlr has no JavaScript engine. MongoDB statements are written in a subset of the `mongosh` syntax that sqlr parses itself (`sqlr-core/src/mongo_shell.rs`). Anything outside it goes through `db.runCommand({...})`, which sends any server command as a document.

## Statements

A statement starts with `db.<collection>`, `db.getCollection("<name>")` or `db["<name>"]` (for names that are not identifiers), followed by one method:

| Method | Notes |
|---|---|
| `find(filter?, projection?)` | chain `.sort({…})`, `.limit(n)`, `.skip(n)`, `.projection({…})`; `.pretty()` and `.toArray()` are accepted and ignored. `findOne(…)` is `find(…)` with `limit 1`. |
| `aggregate([stages], options?)` | `aggregate(stage, stage, …)` is accepted too. A pipeline with `$out` or `$merge` counts as a write. |
| `countDocuments(filter?)`, `estimatedDocumentCount()` | return a count |
| `distinct(field, filter?)` | returns rows |
| `insertOne(doc)`, `insertMany([docs], options?)` | documents without `_id` get a new ObjectId |
| `updateOne`, `updateMany`, `replaceOne`, `findOneAndUpdate` | `(filter, update, options?)`; `replaceOne` refuses update operators |
| `deleteOne(filter)`, `deleteMany(filter)` | `deleteMany({})` is destructive |
| `createIndex(keys, options?)`, `dropIndex(name or keys)`, `getIndexes()` | |
| `drop()` | destructive |

Database-level statements: `db.runCommand({...})`, `db.getCollectionNames()`, `show dbs`, `show collections`, `use <db>` (switches the tab's database; allowed on read-only sessions).

Several statements separated by `;` or by newlines at nesting depth 0 form a script: ⇧⌘↵ runs them one after another, each with its own result. ⌘↵ runs the statement at the caret.

## Literals

Values are relaxed JSON: unquoted keys, single-quoted strings, trailing commas and `//` and `/* */` comments are fine. These helpers are understood:

| Literal | Value |
|---|---|
| `ObjectId("…")` | 24 hex digits, either case |
| `ISODate("…")`, `new Date("…")`, `new Date(ms)` | ISO 8601 only (a date without a zone is UTC); `new Date("01/01/2012")` and `new Date(y, m, d)` are refused with a hint |
| `NumberLong(…)`, `NumberInt(…)`, `NumberDecimal("…")` | integers beyond 32 bits read as 64-bit, never as doubles, so no precision is lost |
| `UUID("…")`, `BinData(subtype, "base64")` | binary |
| `Timestamp(t, i)` | |
| `/pattern/flags` | a `/` inside a character class needs no escape |
| `MinKey`, `MaxKey`, `true`, `false`, `null`, `undefined` | |
| `{$oid: "…"}`, `{$date: …}`, `{$numberDecimal: "…"}`, … | Extended JSON wrappers, as Compass writes them |

## Errors

Unsupported syntax is an error that names the construct and points to `db.runCommand`, for example:

```
db.c.find().forEach(function (d) { print(d) })
→ `.forEach()` is not supported: sqlr runs a subset of the mongo shell without a JavaScript
  engine; use db.runCommand({...}) for anything else
```

Variables (`const x = 1`), arrow functions, unknown `db.` methods (`db.dropDatabase()`) and invalid literals (`0x`, `ObjectId("not-hex")`, an unterminated string) are refused the same way, at the position of the construct.

## Documents in the JSON view

The JSON view shows a document as relaxed Extended JSON and reads it back with the same parser, so every type survives a view → edit → save cycle: an `Int64` that fits in 32 bits keeps its `{"$numberLong": …}` wrapper, a whole double keeps its `.0`, dates outside years 1970–9999 use `{"$date": {"$numberLong": …}}`. Types sqlr does not model (JavaScript code, symbols, DBPointers) are shown but make the document read-only.
