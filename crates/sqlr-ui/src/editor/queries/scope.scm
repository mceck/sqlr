; Relations and CTE names in one statement, for completion scope (tree-sitter-sequel 0.3.11).
; Predicates, if any are added, use Rust regex syntax (`\d`, not Lua's `%d`).
(relation (object_reference schema: (identifier)? @schema name: (identifier) @name) alias: (identifier)? @alias)
(from (object_reference schema: (identifier)? @schema name: (identifier) @name))
(insert (object_reference schema: (identifier)? @schema name: (identifier) @name))
(cte (identifier) @cte)
