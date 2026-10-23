# Document and key-value databases

MongoDB, Elasticsearch and Redis open as first-class tabs with views made for their data. The editing safety you know from SQL (preview, confirmations, read-only profiles) applies here too.

## MongoDB

### Querying

Write statements in the Mongo shell style and run them with ⌘↵:

```js
db.orders.find({ status: "open", total: { $gt: 100 } }).sort({ created: -1 }).limit(20)

db.orders.aggregate([
  { $match: { status: "open" } },
  { $group: { _id: "$customer", total: { $sum: "$total" } } }
])

db.orders.updateOne({ _id: ObjectId("65f1c0de0000000000000001") }, { $set: { status: "closed" } })
```

sqlr reads this syntax itself. The supported methods are `find`, `findOne`, `aggregate`, `countDocuments`, `estimatedDocumentCount`, `distinct`, `insertOne`, `insertMany`, `updateOne`, `updateMany`, `replaceOne`, `findOneAndUpdate`, `deleteOne`, `deleteMany`, `createIndex`, `dropIndex`, `getIndexes` and `drop`, plus `show dbs`, `show collections`, `use <db>` and `db.getCollectionNames()`.

Values can be written the relaxed way: unquoted keys, single quotes, trailing commas and comments are all fine. Helpers such as `ObjectId("...")`, `ISODate("...")`, `NumberLong(...)`, `NumberDecimal("...")`, `UUID("...")` and `/regex/i` are understood.

For anything outside this list, `db.runCommand({ ... })` sends any server command as a document. The complete list, with every literal, is in the [Mongo shell reference](https://github.com/mceck/sqlr/blob/main/docs/tech/mongo-shell-subset.md).

You can write several statements, separated by `;` or new lines. ⇧⌘↵ runs them one after the other, each with its own result.

### Looking at documents

Results can be shown three ways, and you can switch at any time:

- **Table.** One row per document, with a column for each top-level key found in the first page of documents
- **Tree.** Documents as expandable nested values
- **JSON.** The document as text, in Extended JSON

Every type survives a round trip. An `Int64`, a `Decimal128`, a date or an `ObjectId` comes back from the JSON view as the same type it left as.

### Editing

Edit a document in place in the tree or the JSON view, then save it. Documents are identified by `_id`. Index management is in the explorer menu of a collection: create an index (keys and directions, unique, TTL, partial filter) or drop one.

## Elasticsearch

Write requests the way you would in a Kibana console:

```text
GET orders/_search
{
  "query": { "match": { "status": "open" } },
  "size": 20
}
```

Besides the query DSL, sqlr runs **ES|QL** and **SQL** queries. These return tables, so they open in the normal result grid, with sorting, filtering and export like any SQL result.

Search results show as documents, with the same table, tree and JSON views as MongoDB. Documents can be edited and saved. Indices can be created, with their settings and mappings, and dropped, from the explorer.

Large searches fetch more pages as you scroll, using a point in time and `search_after`, so deep results stay consistent.

## Redis

The Redis tab is a key browser.

- **Browsing.** Keys are listed with `SCAN`, in pages, so a big database never stalls the server. Press **Load more** for the next page. Type a pattern such as `user:*` to narrow the list
- **Grouping.** Keys are grouped into folders by a separator, `:` by default, so `user:42:profile` sits inside `user` and `42`
- **Typed values.** Each key shows a view that fits its type: string, hash, list, set, sorted set or stream. Large collections are read in pieces
- **Editing.** Change a value, add or remove members, rename a key, delete a key, and set or remove its TTL (time to live)
- **New key.** **New key** creates one of any type
- **Console.** Type raw Redis commands, with history. Commands that remove data go through your profile's destructive-statement confirmation, and so does `FLUSHDB` and `FLUSHALL`

Redis connections can use TLS and ACL users, and can go through an SSH tunnel.
