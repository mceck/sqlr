[Manual](README.md) › Database families

# Work with documents and Redis keys

MongoDB and Elasticsearch use document results. Redis uses a key browser and command console. Your connection's read-only policy and confirmation settings also apply to these databases.

## Query MongoDB

Install the MongoDB driver from the [Driver Manager](drivers.md#connect-to-mongodb) before connecting. Saved MongoDB connections keep their existing fields, TLS settings and credentials.

Open an editor on a MongoDB connection. Write Mongo shell syntax, then run the current statement with ⌘↵ or the script with ⌘⇧↵.

sqlr parses a subset of shell syntax without running JavaScript. You can use collection reads such as `find`, `findOne`, `aggregate`, `countDocuments`, `estimatedDocumentCount` and `distinct`; writes such as `insertOne`, `insertMany`, `updateOne`, `updateMany`, `replaceOne`, `findOneAndUpdate`, `deleteOne` and `deleteMany`; and index operations such as `createIndex`, `dropIndex` and `getIndexes`.

Use `show dbs`, `show collections`, `use <database>` and `db.getCollectionNames()` to explore. Use `db.getCollection("name")` for names that do not fit dotted notation. `db.runCommand` sends a command document when the shell subset does not have a dedicated method.

The parser accepts unquoted object keys, single-quoted strings, comments and trailing commas. It understands MongoDB literals including `ObjectId`, `ISODate`, `NumberLong`, `NumberDecimal`, `UUID` and regular expressions. JavaScript variables, loops and arbitrary functions are unavailable; rewrite these as a supported query or command document.

## Read and edit document results

Choose a result view above the documents:

| View | Use it for |
|---|---|
| **Table** | Comparing top-level fields across documents |
| **Tree** | Expanding nested objects and arrays |
| **JSON** | Reading or editing a complete document |

Table columns come from the top-level keys found in the first page. Choose **Fetch more** for the next page or **Fetch all** to continue loading within your configured limits.

1. Select the document you want to change.
2. Open its edit action. The JSON editor shows the full document.
3. Change the fields while preserving the identity and typed values.
4. Choose **Save**, or press ⌘S, and answer any confirmation.
5. Check the outcome. sqlr rereads the result after successful changes.

MongoDB uses `_id` to identify a document. Extended JSON preserves values such as dates, `ObjectId`, `Int64` and `Decimal128` when edited and saved. A read-only reason appears when the result cannot be edited.

Use **Insert document** for a new document, and the document's delete action to remove existing ones. MongoDB index actions are available from the collection's explorer menu. Index creation offers key directions, a name, uniqueness, expiry in seconds and a partial filter when applicable.

## Query Elasticsearch

The editor accepts console requests consisting of an HTTP method, a path and an optional JSON body, in the style of the Elasticsearch console. Choose the target index in the path and use its query DSL for searches.

Search results use the same **Table**, **Tree** and **JSON** views as MongoDB. **ES|QL** and Elasticsearch **SQL** queries return tabular results in the normal grid. Supported Elasticsearch versions are 7.17 and 8.x.

Document changes use their index and ID. Refresh a document before editing when another user may have changed it. Index creation and deletion are available in the explorer, with settings and mappings in JSON.

Large search results page using a point in time and `search_after`. If the result expires or paging fails, rerun the query to start a new result.

## Browse Redis keys

1. Open a Redis database from the explorer.
2. Enter a key pattern, for example `customer:*`, and choose **Scan**.
3. Expand prefix groups to find a key. The default grouping separator is `:`.
4. Select a key to open the value view.
5. Choose **Load more** to continue scanning, or **Refresh** (F5 or ⌘R) to restart.

Scanning uses `SCAN`, so the browser does not issue a blocking `KEYS` request. A scan is incremental: keys can change while you browse. A group action such as **Delete loaded keys…** affects the keys already loaded into that group, so check the confirmation count.

The selected key shows its type and a matching view for strings, hashes, lists, sets, sorted sets or streams. Large collections load in pages. Use the value editor's **Save**, or press ⌘S, or the member actions to write changes.

## Create, rename or expire a key

- Choose **New key**, select its type and enter the initial value or members.
- Choose **Rename**, enter **New name**, then choose **Rename to**.
- Enter a positive duration in **TTL**, then choose **Set TTL**. Accepted examples include `90` for seconds, `500ms`, `5m`, `2h` and `1d`. Choose **Persist** to remove expiry.
- Choose **Delete key** to remove the key after confirmation.

TTL means time to live. Expiry removes the key on the server, so refreshing can legitimately show that a previously selected key no longer exists.

The Redis console accepts native commands and keeps command history. Destructive commands, including `FLUSHDB` and `FLUSHALL`, follow the connection's confirmation policy. Redis Cluster is not supported; connect to a standalone server. TLS, ACL users and SSH tunnels are supported through the connection form.

## Related

- [Connections](connections.md): configure authentication, TLS and tunnels.
- [Profiles and transactions](profiles-and-transactions.md): control writes and confirmations.
- [Results and data editing](results-and-editing.md): use tabular Elasticsearch results.

---

← [Administration](administration.md) · [Drivers](drivers.md) →
