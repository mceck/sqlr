use sqlr_driver_sdk::{Capabilities, DriverManifest, Family, FieldSpec, FieldType};

pub(crate) fn manifest() -> DriverManifest {
    DriverManifest {
        id: "duckdb".into(),
        name: "DuckDB".into(),
        family: Family::Relational,
        capabilities: Capabilities {
            transactions: true,
            server_cancel: true,
            schemas: true,
            ..Default::default()
        },
        fields: vec![
            FieldSpec {
                key: "path".into(),
                label: "Database file (:memory: for temporary)".into(),
                ty: FieldType::File {
                    save: false,
                    extensions: vec!["duckdb".into(), "db".into()],
                },
                default: Some(":memory:".into()),
                required: true,
                secret: false,
                group: "Main".into(),
                free_form: false,
            },
            FieldSpec {
                key: "read_only".into(),
                label: "Read-only".into(),
                ty: FieldType::Bool,
                default: Some("false".into()),
                required: false,
                secret: false,
                group: "Main".into(),
                free_form: false,
            },
        ],
        dialect: "duckdb".into(),
        url_schemes: vec!["duckdb".into()],
        default_port: None,
        editing: None,
    }
}
