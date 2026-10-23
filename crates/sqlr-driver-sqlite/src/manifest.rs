//! Static manifest of the SQLite driver: a file instead of host and port, no secrets.

use sqlr_core::{
    Capabilities, DriverManifest, Family, FieldSpec, FieldType, ObjectEditing, ObjectKind,
};

fn field(
    key: &str,
    label: &str,
    ty: FieldType,
    default: Option<&str>,
    required: bool,
) -> FieldSpec {
    FieldSpec {
        free_form: false,
        key: key.into(),
        label: label.into(),
        ty,
        default: default.map(String::from),
        required,
        secret: false,
        group: "Main".into(),
    }
}

fn base_manifest() -> DriverManifest {
    DriverManifest {
        id: "sqlite".into(),
        name: "SQLite".into(),
        family: Family::Relational,
        capabilities: Capabilities {
            explain: true,
            transactions: true,
            server_cancel: true,
            editable_results: true,
            schemas: true,
            dump: true,
            ..Default::default()
        },
        fields: vec![
            field(
                "path",
                "Database file",
                FieldType::File {
                    save: false,
                    extensions: vec!["db".into(), "sqlite".into(), "sqlite3".into()],
                },
                None,
                true,
            ),
            field(
                "read_only",
                "Read-only",
                FieldType::Bool,
                Some("false"),
                false,
            ),
            field(
                "create_if_missing",
                "Create the file if missing",
                FieldType::Bool,
                Some("true"),
                false,
            ),
        ],
        dialect: "sqlite".into(),
        url_schemes: vec!["sqlite".into()],
        default_port: None,
        editing: Some(editing()),
    }
}

/// Tables, views, indexes and triggers; no comments. Table changes `ALTER TABLE` cannot
/// make rebuild the table.
fn editing() -> ObjectEditing {
    use ObjectKind::*;
    let creatable = vec![Table, View, Index, Trigger];
    ObjectEditing {
        alter: vec![Table, View],
        drop: creatable.clone(),
        rename: vec![Table],
        create: creatable,
        transactional_ddl: true,
        column_types: ["INTEGER", "REAL", "TEXT", "BLOB", "NUMERIC", "ANY"]
            .iter()
            .map(|t| t.to_string())
            .collect(),
        comments: false,
        column_order: true,
    }
}

pub(crate) fn manifest() -> DriverManifest {
    let mut m = base_manifest();
    m.fields.extend(sqlr_core::driver_property_fields("sqlite"));
    m
}
