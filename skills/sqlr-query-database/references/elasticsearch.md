# Elasticsearch in sqlr

`run_query` takes one request in the Kibana Dev Tools console syntax:

```
GET /orders/_search
{
  "size": 20,
  "query": { "term": { "status": "open" } }
}
```

- The first line is `METHOD path[?query]`; an optional JSON body follows. `_bulk` and `_msearch` take NDJSON, one value per line.
- `ESQL <query>` (servers 8.11 and later) and `SQL <query>` return tabular Results.
- Bodies accept `//`, `/* */` and `#` comments and `"""…"""` multi-line strings.

Searches: set `size` explicitly. A `_search` without `size` pages through every hit, which can be large. A body with aggregations or `size: 0` returns the response itself. Hits come back as `{_index, _id, …_source}`.

Classification: `GET`, `HEAD`, ES|QL, SQL, `_search`, `_count`, `_mget` and similar read. `_doc`, `_bulk`, `_update`, `_update_by_query` and `_reindex` write. `_mapping`, `_settings`, aliases, templates and `PUT /<index>` are DDL. `DELETE /<index>` and `_delete_by_query` without a query or with `match_all` are destructive. Paths with `.`/`..` segments or percent-escapes are refused.

Elasticsearch has no transactions: writes apply at once.

Use `browse` to list indices (health, document count, size), aliases and data streams.
