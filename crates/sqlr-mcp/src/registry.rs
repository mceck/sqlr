//! The single source of truth for the tool names, descriptions and input schemas.
//! `tools/list` is generated from this list, and the sqlr skills are checked against it.

use std::sync::Arc;

use rmcp::model::{JsonObject, Tool};
use serde_json::{Value, json};

pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
}

pub static TOOLS: std::sync::LazyLock<Vec<ToolSpec>> = std::sync::LazyLock::new(|| {
    vec![
        ToolSpec {
            name: "config_list",
            description: "Discover application configuration. Start database work with target=connections to get exposed connection IDs, drivers and read/write access. Connection values and secrets are never returned. Use target=drivers for driver fields before writing connection configuration.",
            schema: json!({
                "type": "object",
                "required": ["target"],
                "properties": {
                    "target": {"type": "string", "enum": ["connections", "folders", "profiles", "settings", "keymap", "drivers"], "description": "Configuration collection to read; connections includes only connections exposed to agents."},
                    "driver_id": {"type": "string", "description": "Optional manifest ID filter, used only with target=drivers; obtain IDs from connections or the unfiltered driver list."}
                }
            }),
        },
        ToolSpec {
            name: "config_write",
            description: "Create or update application configuration, not database data. Read the matching config_list collection first. Connection values/secrets are write-only; a new connection starts unexposed, and an existing one must be exposed or created by this session. Exposure is human-only and safety settings can only be tightened.",
            schema: json!({
                "type": "object",
                "required": ["target", "payload"],
                "properties": {
                    "target": {"type": "string", "enum": ["connection", "folder", "profile", "setting", "keymap"], "description": "Singular configuration kind to write; each kind has a different payload."},
                    "payload": {"type": "object", "description": "connection: create with name, driver and optional values, secrets, group, profile, color, save_secrets; update with id plus fields to patch, remove_values and remove_secrets; driver cannot change and agent_access is rejected. folder: {action:\"create\", name}, {action:\"rename\", from, to} or {action:\"move\", name, target:{at:\"end\"|\"before\"|\"after\"|\"into\", id}}. profile: create with name, update with id; optional color, commit_mode (auto/smart/manual), confirm_writes, confirm_destructive, read_only, idle_tx_timeout_min. setting: id, level {name:\"global\"} or {name:\"profile\"|\"connection\", id}, and value or reset:true. keymap: action plus chords:[\"Cmd+Shift+P\"] or reset:true."}
                }
            }),
        },
        ToolSpec {
            name: "config_delete",
            description: "Delete saved application configuration, not database objects. Obtain the id from config_list (use the folder path for folders). Always waits for human approval in sqlr; denial or timeout returns an error. A connection must be exposed or created by this session; a folder's connections and subfolders move to its parent; a profile in use is refused.",
            schema: json!({
                "type": "object",
                "required": ["target", "id"],
                "properties": {
                    "target": {"type": "string", "enum": ["connection", "folder", "profile"]},
                    "id": {"type": "string"}
                }
            }),
        },
        ToolSpec {
            name: "test_connection",
            description: "Check that a connection opens, as the connection dialog's Test does, without showing it in the Explorer. Use it after creating or fixing a connection. Returns {ok:true, server_version} or {ok:false, error:{code, message}} with code auth, connection, tls or timeout; missing secret fields answer secret_required naming them. Works on exposed connections and on those this session created.",
            schema: json!({
                "type": "object",
                "required": ["connection_id"],
                "properties": {
                    "connection_id": {"type": "string", "description": "Connection ID: an exposed one from config_list(target=connections), or one this session created with config_write."}
                }
            }),
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
        },
        ToolSpec {
            name: "get_ddl",
            description: "Read the CREATE script of a catalog object (table, view, index, routine, sequence…) exactly as sqlr's DDL viewer shows it. Pass a node path returned by browse unchanged. Read-only; relational connections only.",
            schema: json!({
                "type": "object",
                "required": ["connection_id", "path"],
                "properties": {
                    "connection_id": {"type": "string", "description": "Exposed connection ID returned by config_list(target=connections), not its display name."},
                    "path": {"type": "array", "items": {"type": "string"}, "description": "Full node path as returned by browse."}
                }
            }),
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
        },
        ToolSpec {
            name: "explain_query",
            description: "Show the plan of one SQL statement on the agent session: execution and planning time, cost, estimated and actual rows, plain-language warnings, the operator mix and the raw plan (up to 256 KiB). analyze executes the statement: refused for a write on Read exposure, and a write waits for approval and runs in a transaction that is always rolled back. Answers busy while the agent session has an open transaction, unsupported when the driver has no plans.",
            schema: json!({
                "type": "object",
                "required": ["connection_id", "query"],
                "properties": {
                    "connection_id": {"type": "string", "description": "Exposed connection ID returned by config_list(target=connections), not its display name."},
                    "query": {"type": "string", "description": "Exactly one SQL statement: a query or a data change."},
                    "database": {"type": "string", "description": "Optional database execution context discovered via browse."},
                    "analyze": {"type": "boolean", "default": false, "description": "Execute the statement to measure actual rows and times."}
                }
            }),
        },
        ToolSpec {
            name: "get_query_result",
            description: "Poll a running result or retrieve another page using its query_id: one from run_query, or a tab result id from run_query_tab or get_query_tab (same envelope). Do not rerun the statement to fetch more rows. Offset is zero-based; advance it by the number of rows/documents actually returned, since the byte budget can shorten a page. run_query results expire after five idle minutes or close_result; tab results last while the tab shows them.",
            schema: json!({
                "type": "object",
                "required": ["query_id"],
                "properties": {
                    "query_id": {"type": "string", "description": "Opaque query_id returned by run_query, run_query_tab or get_query_tab; not a tab ID or a connection ID."},
                    "offset": {"type": "integer", "minimum": 0, "default": 0, "description": "Zero-based row/document offset. For the next page, add the actual returned count to the previous offset; keep the offset when polling an empty running page."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 200, "description": "Requested page size; the 256 KiB data budget may return fewer rows/documents."}
                }
            }),
        },
        ToolSpec {
            name: "cancel_query",
            description: "Request cancellation of a run_query operation by query_id; repeated calls are safe. Use get_query_result to inspect its status afterward and close_result to release the retained result.",
            schema: json!({
                "type": "object",
                "required": ["query_id"],
                "properties": {"query_id": {"type": "string", "description": "Opaque query_id returned by run_query; not a tab ID or a connection ID."}}
            }),
        },
        ToolSpec {
            name: "close_result",
            description: "Release a run_query result and its retained memory when finished. Later get_query_result calls for this id return not_found. Repeated closes are safe: closed=true on the first removal, false if already absent. A tab's result belongs to the tab: closing it answers invalid.",
            schema: json!({
                "type": "object",
                "required": ["query_id"],
                "properties": {"query_id": {"type": "string", "description": "Opaque query_id returned by run_query; not a tab ID or a connection ID."}}
            }),
        },
        ToolSpec {
            name: "search_history",
            description: "Search the statements run earlier (by the person or agents) on exposed connections, newest first, by case-insensitive substring of the SQL as the History panel does. Returns SQL, connection, database, start time, duration and outcome. Default limit 50, maximum 200.",
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Case-insensitive substring of the SQL; omit to list the newest entries."},
                    "connection_id": {"type": "string", "description": "Exposed connection ID returned by config_list(target=connections), not its display name."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50, "description": "Maximum entries to return."}
                }
            }),
        },
        ToolSpec {
            name: "list_query_tabs",
            description: "Discover existing SQL editor tabs and their numeric tab IDs. Use get_query_tab to read text and the current revision before editing or choosing what to run. This tool does not create tabs or list retained run_query results.",
            schema: json!({"type": "object", "properties": {}}),
        },
        ToolSpec {
            name: "get_query_tab",
            description: "Read an existing SQL editor tab's text, connection/database, revision, busy state, caret and selection. For an exposed connection it adds last_run_query_ids (the tab's most recent run, whoever started it; page them with get_query_result), transaction_open and grid_selection (selected result cells as {column, row, value}). Obtain tab_id from list_query_tabs. Use the returned revision as expected_revision for edit_query_tab; positions are character offsets.",
            schema: json!({
                "type": "object",
                "required": ["tab_id"],
                "properties": {"tab_id": {"type": "integer", "description": "Numeric ID of an existing SQL editor tab from list_query_tabs."}}
            }),
        },
        ToolSpec {
            name: "open_query_tab",
            description: "Open a new unsaved SQL editor tab with text for the person, beside the active tab, and make it active. Runs nothing. Use it to propose a query instead of overwriting one of the person's tabs. Returns tab_id and revision. One session opens at most five tabs (counted with show_in_app); more answers busy.",
            schema: json!({
                "type": "object",
                "required": ["connection_id", "text"],
                "properties": {
                    "connection_id": {"type": "string", "description": "Exposed connection ID returned by config_list(target=connections), not its display name."},
                    "database": {"type": "string", "description": "Optional database the tab targets, discovered via browse."},
                    "text": {"type": "string", "description": "The SQL the tab opens with."},
                    "title": {"type": "string", "description": "Optional tab title."}
                }
            }),
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
        },
        ToolSpec {
            name: "run_query_tab",
            description: "Execute SQL already in an existing editor tab using that tab's connection/database and the UI execution flow; results land in the person's grid. Read get_query_tab first to verify text, selection, busy state and transaction_open. Choose the whole script, a zero-based statement index or the current selection. Writes the profile asks to confirm wait for the person's approval; the run starts once. Returns {tab_id, status, query_ids}: one id per executed statement, pageable with get_query_result.",
            schema: json!({
                "type": "object",
                "required": ["tab_id", "target"],
                "properties": {
                    "tab_id": {"type": "integer", "description": "Numeric ID of an existing SQL editor tab from list_query_tabs."},
                    "target": {"type": ["string", "object"], "description": "Use \"script\" for the entire tab, \"selection\" for its current selection, or {\"statement\":0} for the first parsed statement (zero-based)."},
                    "max_rows": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 200, "description": "Initial row fetch size for the tab's execution."}
                }
            }),
        },
        ToolSpec {
            name: "show_in_app",
            description: "Show something to the person in sqlr instead of describing it; read-only. view=explorer expands and selects a browse path; ddl opens the DDL viewer of a path; table_data opens a table or view's data tab; er_diagram opens an ER diagram of tables; explain shows a plan in an existing tab's Plan pane (tab_id, optional statement index) or in a new tab with query. analyze follows explain_query's rules. Returns tab_id when a tab opens; tab-opening views count toward the five-tab limit.",
            schema: json!({
                "type": "object",
                "required": ["connection_id", "view"],
                "properties": {
                    "connection_id": {"type": "string", "description": "Exposed connection ID returned by config_list(target=connections), not its display name."},
                    "view": {"type": "string", "enum": ["explorer", "ddl", "table_data", "er_diagram", "explain"]},
                    "path": {"type": "array", "items": {"type": "string"}, "description": "explorer, ddl, table_data: full node path as returned by browse."},
                    "database": {"type": "string", "description": "er_diagram and explain with query: optional database."},
                    "schema": {"type": "string", "description": "er_diagram: the schema of the tables; defaults to the driver's default schema."},
                    "tables": {"type": "array", "items": {"type": "string"}, "description": "er_diagram: table names."},
                    "tab_id": {"type": "integer", "description": "explain: an existing SQL editor tab on this connection."},
                    "statement": {"type": "integer", "minimum": 0, "description": "explain with tab_id: zero-based statement index; omit for the statement at the caret."},
                    "query": {"type": "string", "description": "explain: one statement to open in a new tab and explain."},
                    "analyze": {"type": "boolean", "default": false, "description": "explain: execute the statement to measure it."}
                }
            }),
        },
        ToolSpec {
            name: "list_snippets",
            description: "Discover saved snippet IDs, names, folders and connection scopes. SQL text is omitted; use get_snippet to read it before editing or executing. Saving a snippet does not execute it.",
            schema: json!({"type": "object", "properties": {}}),
        },
        ToolSpec {
            name: "get_snippet",
            description: "Read a saved snippet including its SQL, folder and optional connection scope. Obtain id from list_snippets. To execute, pass its SQL to run_query with an exposed connection; this tool only reads the snippet.",
            schema: json!({
                "type": "object",
                "required": ["id"],
                "properties": {"id": {"type": "string"}}
            }),
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
        },
        ToolSpec {
            name: "delete_snippet",
            description: "Permanently delete a saved snippet by id from list_snippets. Always waits for human approval in sqlr; denial or timeout returns an error. Does not execute the snippet or modify database objects.",
            schema: json!({
                "type": "object",
                "required": ["id"],
                "properties": {"id": {"type": "string"}}
            }),
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
    fn tools_have_unique_names_and_object_schemas() {
        let mut names = std::collections::HashSet::new();
        for tool in TOOLS.iter() {
            assert!(names.insert(tool.name), "duplicate {}", tool.name);
            assert_eq!(tool.schema["type"], "object", "{}", tool.name);
        }
    }
}
