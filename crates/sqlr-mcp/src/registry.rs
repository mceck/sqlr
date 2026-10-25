//! The single source of truth for the tool names, descriptions and input schemas. The
//! guide and `tools/list` are both generated from this list.

use std::sync::Arc;

use rmcp::model::{JsonObject, Tool};
use serde_json::{Value, json};

pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
    /// A compact argument list for the guide.
    pub arguments: &'static str,
}

pub static TOOLS: std::sync::LazyLock<Vec<ToolSpec>> = std::sync::LazyLock::new(|| {
    vec![
        ToolSpec {
            name: "get_guide",
            description: "Required first call in every new MCP session: call get_guide with {} and read the overview before calling any other tool. Returns Markdown with the discovery workflow, tool selection, result paging, errors and access rules. Request a topic for detailed reference.",
            schema: json!({
                "type": "object",
                "properties": {
                    "topic": {"type": "string", "enum": ["overview", "security", "tools", "recipes", "terminal"], "default": "overview", "description": "Omit for the required first-call overview. tools covers arguments and configuration payloads; recipes provides JSON call sequences; security covers permissions; terminal covers human setup."}
                }
            }),
            arguments: "optional topic",
        },
        ToolSpec {
            name: "config_list",
            description: "Discover application configuration. Start database work with target=connections to get exposed connection IDs, drivers and read/write access. Connection values and secrets are never returned. Use target=drivers for driver fields before writing connection configuration.",
            schema: json!({
                "type": "object",
                "required": ["target"],
                "properties": {
                    "target": {"type": "string", "enum": ["connections", "folders", "profiles", "settings", "keymap", "tasks", "drivers"], "description": "Configuration collection to read; connections includes only connections exposed to agents."},
                    "driver_id": {"type": "string", "description": "Optional manifest ID filter, used only with target=drivers; obtain IDs from connections or the unfiltered driver list."}
                }
            }),
            arguments: "target; optional driver_id",
        },
        ToolSpec {
            name: "config_write",
            description: "Create or update application configuration, not database data. Read the matching config_list collection and get_guide(topic=tools) for payload shapes first. Connection values/secrets are write-only; new connections remain unexposed. Exposure is human-only and safety settings can only be tightened.",
            schema: json!({
                "type": "object",
                "required": ["target", "payload"],
                "properties": {
                    "target": {"type": "string", "enum": ["connection", "folder", "profile", "setting", "keymap"], "description": "Singular configuration kind to write; each kind has a different payload."},
                    "payload": {"type": "object", "description": "Target-specific fields; see get_guide(topic=tools). Connection creation needs name and driver; updates use id. Driver fields go in values, credentials in secrets. Do not include agent_access."}
                }
            }),
            arguments: "target, payload",
        },
        ToolSpec {
            name: "config_delete",
            description: "Delete saved application configuration, not database objects. Obtain the id from config_list (use the folder path for folders). Always waits for human approval in sqlr; denial or timeout returns an error. Read the guide for consequences before deleting.",
            schema: json!({
                "type": "object",
                "required": ["target", "id"],
                "properties": {
                    "target": {"type": "string", "enum": ["connection", "folder", "profile", "task"]},
                    "id": {"type": "string"}
                }
            }),
            arguments: "target, id",
        },
        ToolSpec {
            name: "browse",
            description: "Explore an exposed connection's database/catalog tree. Omit path or use [] for root children, then pass a returned node's full path unchanged to expand it. Returns names, kinds, paths and has_children. Use describe_object for relational columns and keys, run_query for data.",
            schema: json!({
                "type": "object",
                "required": ["connection_id"],
                "properties": {
                    "connection_id": {"type": "string", "description": "Exposed connection ID returned by config_list(target=connections), not its display name."},
                    "path": {"type": "array", "items": {"type": "string"}, "default": [], "description": "Full catalog path as returned by browse; omit or use [] for the root. Do not guess segments or concatenate a dotted SQL name."}
                }
            }),
            arguments: "connection_id; optional path",
        },
        ToolSpec {
            name: "describe_object",
            description: "Inspect a relational table's committed columns, types, defaults and keys before writing SQL. Connects automatically; no preliminary query is required. Get the exact table name and database/schema context from browse. Metadata uses the catalog session, so uncommitted agent changes may not be visible. Does not return rows; non-relational connections return unsupported.",
            schema: json!({
                "type": "object",
                "required": ["connection_id", "name"],
                "properties": {
                    "connection_id": {"type": "string", "description": "Exposed connection ID returned by config_list(target=connections), not its display name."},
                    "database": {"type": "string", "description": "Database containing the table; use the context discovered from the catalog."},
                    "schema": {"type": "string", "description": "Schema containing the table, when the driver supports schemas."},
                    "name": {"type": "string", "description": "Table name only; pass database and schema separately."}
                }
            }),
            arguments: "connection_id, name; optional database, schema",
        },
        ToolSpec {
            name: "search_objects",
            description: "Find relational table/view names by a case-insensitive substring in each connection's default database, across exposed connections or one connection_id. Searches names, not row data, and skips non-relational or unreachable connections. Use browse to explore other databases. Returns matches and catalog context; default limit 50, maximum 200.",
            schema: json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {"type": "string", "description": "Case-insensitive substring of an object name, not SQL or a query over table rows."},
                    "connection_id": {"type": "string", "description": "Exposed connection ID returned by config_list(target=connections), not its display name."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50, "description": "Maximum object matches to return."}
                }
            }),
            arguments: "query; optional connection_id, limit",
        },
        ToolSpec {
            name: "run_query",
            description: "Execute one SQL statement or driver-native command on an exposed connection without using an editor tab. Inspect the driver/catalog first. Returns kind, status and data; use query_id, when present, to poll/page with get_query_result, then close_result. A timed-out key-value command may return running without an ID: report the uncertain outcome, do not resubmit it. Writes follow exposure/profile approvals; use run_query_tab for an existing editor script.",
            schema: json!({
                "type": "object",
                "required": ["connection_id", "query"],
                "properties": {
                    "connection_id": {"type": "string", "description": "Exposed connection ID returned by config_list(target=connections), not its display name."},
                    "query": {"type": "string", "description": "Exactly one SQL statement for relational drivers, or one command in the driver's native syntax for document/key-value drivers. SQL scripts are rejected."},
                    "database": {"type": "string", "description": "Optional database execution context discovered via browse; omit to use the connection default."},
                    "max_rows": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 200, "description": "Rows/documents requested for the first page, not a total query limit. The page also has a 256 KiB data budget; use SQL LIMIT or native syntax to bound total work."},
                    "timeout_s": {"type": "integer", "minimum": 1, "maximum": 120, "default": 30, "description": "Time to wait for execution. A running SQL/document result includes query_id for polling/cancellation. Key-value commands may return running without an ID; inspect sqlr and do not resubmit a command with an uncertain outcome."}
                }
            }),
            arguments: "connection_id, query; optional database, max_rows, timeout_s",
        },
        ToolSpec {
            name: "get_query_result",
            description: "Poll a running run_query result or retrieve another page using its query_id. Do not rerun the statement to fetch more rows. Offset is zero-based; advance it by the number of rows/documents actually returned, since the byte budget can shorten a page. Results expire after five idle minutes or close_result.",
            schema: json!({
                "type": "object",
                "required": ["query_id"],
                "properties": {
                    "query_id": {"type": "string", "description": "Opaque query_id returned by run_query; not a tab ID or a connection ID."},
                    "offset": {"type": "integer", "minimum": 0, "default": 0, "description": "Zero-based row/document offset. For the next page, add the actual returned count to the previous offset; keep the offset when polling an empty running page."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 200, "description": "Requested page size; the 256 KiB data budget may return fewer rows/documents."}
                }
            }),
            arguments: "query_id; optional offset, limit",
        },
        ToolSpec {
            name: "cancel_query",
            description: "Request cancellation of a run_query operation by query_id; repeated calls are safe. Use get_query_result to inspect its status afterward and close_result to release the retained result.",
            schema: json!({
                "type": "object",
                "required": ["query_id"],
                "properties": {"query_id": {"type": "string", "description": "Opaque query_id returned by run_query; not a tab ID or a connection ID."}}
            }),
            arguments: "query_id",
        },
        ToolSpec {
            name: "close_result",
            description: "Release a run_query result and its retained memory when finished. Later get_query_result calls for this id return not_found. Repeated closes are safe: closed=true on the first removal, false if already absent.",
            schema: json!({
                "type": "object",
                "required": ["query_id"],
                "properties": {"query_id": {"type": "string", "description": "Opaque query_id returned by run_query; not a tab ID or a connection ID."}}
            }),
            arguments: "query_id",
        },
        ToolSpec {
            name: "list_query_tabs",
            description: "Discover existing SQL editor tabs and their numeric tab IDs. Use get_query_tab to read text and the current revision before editing or choosing what to run. This tool does not create tabs or list retained run_query results.",
            schema: json!({"type": "object", "properties": {}}),
            arguments: "none",
        },
        ToolSpec {
            name: "get_query_tab",
            description: "Read an existing SQL editor tab's text, connection/database, revision, busy state, caret and selection. Obtain tab_id from list_query_tabs. Use the returned revision as expected_revision for edit_query_tab; positions are character offsets.",
            schema: json!({
                "type": "object",
                "required": ["tab_id"],
                "properties": {"tab_id": {"type": "integer", "description": "Numeric ID of an existing SQL editor tab from list_query_tabs."}}
            }),
            arguments: "tab_id",
        },
        ToolSpec {
            name: "edit_query_tab",
            description: "Edit an existing SQL tab without executing it. Read it with get_query_tab first and supply its revision. Use edit={replace: text} for all text or edit={edits: [{start, end, text}]} for character ranges. On conflict, reread and recompute the edit. The person sees an undoable change.",
            schema: json!({
                "type": "object",
                "required": ["tab_id", "expected_revision", "edit"],
                "properties": {
                    "tab_id": {"type": "integer", "description": "Numeric ID of an existing SQL editor tab from list_query_tabs."},
                    "expected_revision": {"type": "integer", "description": "Exact revision from the latest get_query_tab; a stale revision returns conflict without overwriting the person's changes."},
                    "edit": {"type": "object", "description": "Use {\"replace\":\"new SQL\"} or {\"edits\":[{\"start\":0,\"end\":6,\"text\":\"SELECT\"}]}. Ranges use zero-based character offsets with an exclusive end, not byte offsets."}
                }
            }),
            arguments: "tab_id, expected_revision, edit",
        },
        ToolSpec {
            name: "run_query_tab",
            description: "Execute SQL already in an existing editor tab using that tab's connection/database and the UI execution flow. Read get_query_tab first to verify text, selection and busy state. Choose the whole script, a zero-based statement index or the current selection. Returns the UI execution response; run_query provides directly pageable MCP results.",
            schema: json!({
                "type": "object",
                "required": ["tab_id", "target"],
                "properties": {
                    "tab_id": {"type": "integer", "description": "Numeric ID of an existing SQL editor tab from list_query_tabs."},
                    "target": {"type": ["string", "object"], "description": "Use \"script\" for the entire tab, \"selection\" for its current selection, or {\"statement\":0} for the first parsed statement (zero-based)."},
                    "max_rows": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 200, "description": "Initial row fetch size for the tab's execution."}
                }
            }),
            arguments: "tab_id, target; optional max_rows",
        },
        ToolSpec {
            name: "list_snippets",
            description: "Discover saved snippet IDs, names, folders and connection scopes. SQL text is omitted; use get_snippet to read it before editing or executing. Saving a snippet does not execute it.",
            schema: json!({"type": "object", "properties": {}}),
            arguments: "none",
        },
        ToolSpec {
            name: "get_snippet",
            description: "Read a saved snippet including its SQL, folder and optional connection scope. Obtain id from list_snippets. To execute, pass its SQL to run_query with an exposed connection; this tool only reads the snippet.",
            schema: json!({
                "type": "object",
                "required": ["id"],
                "properties": {"id": {"type": "string"}}
            }),
            arguments: "id",
        },
        ToolSpec {
            name: "write_snippet",
            description: "Save a snippet without executing SQL. Supply id to update a known snippet; without id an existing snippet with the same name is replaced, otherwise one is created. Name and SQL are required even for updates. Omitted folder resets to root and omitted conn_id clears connection scope; preserve them explicitly when editing.",
            schema: json!({
                "type": "object",
                "required": ["name", "sql"],
                "properties": {
                    "id": {"type": "string"},
                    "name": {"type": "string", "description": "Non-empty snippet name; also used to find an existing snippet when id is omitted."},
                    "folder": {"type": "string", "description": "Slash-separated snippet folder, created implicitly; omit for root. Include the current folder to preserve it on update."},
                    "conn_id": {"type": "string", "description": "Optional connection scope (this field is named conn_id, not connection_id). Omit to save an unscoped snippet."},
                    "sql": {"type": "string", "description": "Full SQL text to save; required on both creation and update. It is not executed."}
                }
            }),
            arguments: "name, sql; optional id, folder, conn_id",
        },
        ToolSpec {
            name: "delete_snippet",
            description: "Permanently delete a saved snippet by id from list_snippets. Always waits for human approval in sqlr; denial or timeout returns an error. Does not execute the snippet or modify database objects.",
            schema: json!({
                "type": "object",
                "required": ["id"],
                "properties": {"id": {"type": "string"}}
            }),
            arguments: "id",
        },
    ]
});

pub fn tool_spec(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|tool| tool.name == name)
}

pub fn tools() -> Vec<Tool> {
    TOOLS
        .iter()
        .map(|spec| {
            let schema: JsonObject = spec.schema.as_object().cloned().unwrap_or_default();
            Tool::new(spec.name, spec.description, Arc::new(schema))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_nineteen_tools_with_unique_names() {
        assert_eq!(TOOLS.len(), 19);
        let mut names = std::collections::HashSet::new();
        for tool in TOOLS.iter() {
            assert!(names.insert(tool.name), "duplicate {}", tool.name);
            assert_eq!(tool.schema["type"], "object", "{}", tool.name);
        }
    }
}
