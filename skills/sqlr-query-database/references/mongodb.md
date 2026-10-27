# MongoDB in sqlr

sqlr has no JavaScript engine. `run_query` takes one statement in a subset of `mongosh` syntax; anything else goes through `db.runCommand({...})`.

A statement starts with `db.<collection>`, `db.getCollection("<name>")` or `db["<name>"]`, then one method:

- `find(filter?, projection?)` with `.sort({…})`, `.limit(n)`, `.skip(n)`, `.projection({…})`; `findOne(…)`;
- `aggregate([stages], options?)`; `$out` and `$merge` make it a write;
- `countDocuments(filter?)`, `estimatedDocumentCount()`, `distinct(field, filter?)`;
- `insertOne`, `insertMany`, `updateOne`, `updateMany`, `replaceOne`, `findOneAndUpdate`, `deleteOne`, `deleteMany`;
- `createIndex`, `dropIndex`, `getIndexes()`, `drop()`.

Database level: `db.runCommand({...})`, `db.getCollectionNames()`, `show dbs`, `show collections`.

Values are relaxed JSON (unquoted keys, single quotes, trailing commas, comments) with `ObjectId("…")`, `ISODate("…")`, `NumberLong(…)`, `NumberDecimal("…")`, `UUID("…")`, `BinData(…)`, `Timestamp(…)`, regular expressions and Extended JSON wrappers. Dates must be ISO 8601: `ISODate("2024-01-31T00:00:00Z")`.

Not accepted: variables, functions, `.forEach()`, and `db.` methods outside the list; the error points to `db.runCommand`.

Always pass `.limit(n)` to `find` and put a `$limit` stage in exploratory pipelines. `deleteMany({})`, `updateMany({}, …)`, `drop()` and `dropIndex` are destructive and usually need the user's Approval. MongoDB has no transactions here: writes apply at once.

Results are `document` Results: JSON documents with their key order kept; types outside the JSON model come back in Extended JSON (`{"$oid": …}`, `{"$date": …}`).

Use `browse` to list databases, collections, views and a collection's indexes.
