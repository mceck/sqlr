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
            description: "Markdown guide: overview, security, tools, recipes or terminal.",
            schema: json!({
                "type": "object",
                "properties": {
                    "topic": {"type": "string", "enum": ["overview", "security", "tools", "recipes", "terminal"]}
                }
            }),
            arguments: "optional topic",
        },
        ToolSpec {
            name: "config_list",
            description: "List connection metadata, folders, profiles, settings, keymap, tasks or driver manifests.",
            schema: json!({
                "type": "object",
                "required": ["target"],
                "properties": {
                    "target": {"type": "string", "enum": ["connections", "folders", "profiles", "settings", "keymap", "tasks", "drivers"]},
                    "driver_id": {"type": "string"}
                }
            }),
            arguments: "target; optional driver_id",
        },
        ToolSpec {
            name: "config_write",
            description: "Create or patch a connection, folder, profile, setting, keymap action or task.",
            schema: json!({
                "type": "object",
                "required": ["target", "payload"],
                "properties": {
                    "target": {"type": "string", "enum": ["connection", "folder", "profile", "setting", "keymap", "task"]},
                    "payload": {"type": "object"}
                }
            }),
            arguments: "target, payload",
        },
        ToolSpec {
            name: "config_delete",
            description: "Delete a connection, folder, profile or task after an approval.",
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
            description: "Load and return the children of a catalog path on an exposed connection.",
            schema: json!({
                "type": "object",
                "required": ["connection_id"],
                "properties": {
                    "connection_id": {"type": "string"},
                    "path": {"type": "array", "items": {"type": "string"}}
                }
            }),
            arguments: "connection_id; optional path",
        },
        ToolSpec {
            name: "describe_object",
            description: "Columns, keys and types of a relational table; other families return unsupported.",
            schema: json!({
                "type": "object",
                "required": ["connection_id", "name"],
                "properties": {
                    "connection_id": {"type": "string"},
                    "database": {"type": "string"},
                    "schema": {"type": "string"},
                    "name": {"type": "string"}
                }
            }),
            arguments: "connection_id, name; optional database, schema",
        },
        ToolSpec {
            name: "search_objects",
            description: "Search the catalogs of exposed connections with a limit.",
            schema: json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {"type": "string"},
                    "connection_id": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200}
                }
            }),
            arguments: "query; optional connection_id, limit",
        },
        ToolSpec {
            name: "run_query",
            description: "Run exactly one statement or native command on an exposed connection.",
            schema: json!({
                "type": "object",
                "required": ["connection_id", "query"],
                "properties": {
                    "connection_id": {"type": "string"},
                    "query": {"type": "string"},
                    "database": {"type": "string"},
                    "max_rows": {"type": "integer", "minimum": 1, "maximum": 1000},
                    "timeout_s": {"type": "integer", "minimum": 1, "maximum": 120}
                }
            }),
            arguments: "connection_id, query; optional database, max_rows, timeout_s",
        },
        ToolSpec {
            name: "get_query_result",
            description: "Page an existing result by id.",
            schema: json!({
                "type": "object",
                "required": ["query_id"],
                "properties": {
                    "query_id": {"type": "string"},
                    "offset": {"type": "integer", "minimum": 0},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000}
                }
            }),
            arguments: "query_id; optional offset, limit",
        },
        ToolSpec {
            name: "cancel_query",
            description: "Cancel a query; idempotent.",
            schema: json!({
                "type": "object",
                "required": ["query_id"],
                "properties": {"query_id": {"type": "string"}}
            }),
            arguments: "query_id",
        },
        ToolSpec {
            name: "close_result",
            description: "Release a result and its memory; idempotent.",
            schema: json!({
                "type": "object",
                "required": ["query_id"],
                "properties": {"query_id": {"type": "string"}}
            }),
            arguments: "query_id",
        },
        ToolSpec {
            name: "list_query_tabs",
            description: "List the open SQL query tabs.",
            schema: json!({"type": "object", "properties": {}}),
            arguments: "none",
        },
        ToolSpec {
            name: "get_query_tab",
            description: "Read a tab's text, revision, caret and selection.",
            schema: json!({
                "type": "object",
                "required": ["tab_id"],
                "properties": {"tab_id": {"type": "integer"}}
            }),
            arguments: "tab_id",
        },
        ToolSpec {
            name: "edit_query_tab",
            description: "Replace a tab's text or apply range edits with a revision check.",
            schema: json!({
                "type": "object",
                "required": ["tab_id", "expected_revision", "edit"],
                "properties": {
                    "tab_id": {"type": "integer"},
                    "expected_revision": {"type": "integer"},
                    "edit": {"type": "object"}
                }
            }),
            arguments: "tab_id, expected_revision, edit",
        },
        ToolSpec {
            name: "run_query_tab",
            description: "Run the tab's script, a statement index or its selection.",
            schema: json!({
                "type": "object",
                "required": ["tab_id", "target"],
                "properties": {
                    "tab_id": {"type": "integer"},
                    "target": {"type": ["string", "object"]},
                    "max_rows": {"type": "integer", "minimum": 1, "maximum": 1000}
                }
            }),
            arguments: "tab_id, target; optional max_rows",
        },
        ToolSpec {
            name: "list_snippets",
            description: "List snippet metadata (never the SQL).",
            schema: json!({"type": "object", "properties": {}}),
            arguments: "none",
        },
        ToolSpec {
            name: "get_snippet",
            description: "Read one snippet, including its SQL.",
            schema: json!({
                "type": "object",
                "required": ["id"],
                "properties": {"id": {"type": "string"}}
            }),
            arguments: "id",
        },
        ToolSpec {
            name: "write_snippet",
            description: "Create or update a snippet; folders are created implicitly.",
            schema: json!({
                "type": "object",
                "required": ["name", "sql"],
                "properties": {
                    "id": {"type": "string"},
                    "name": {"type": "string"},
                    "folder": {"type": "string"},
                    "conn_id": {"type": "string"},
                    "sql": {"type": "string"}
                }
            }),
            arguments: "name, sql; optional id, folder, conn_id",
        },
        ToolSpec {
            name: "delete_snippet",
            description: "Delete a snippet after an approval.",
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
