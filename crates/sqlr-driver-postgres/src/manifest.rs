//! Static manifest of the PostgreSQL driver.

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
        secret: ty == FieldType::Password,
        ty,
        default: default.map(String::from),
        required,
        group: "Main".into(),
    }
}

fn tls_field(key: &str, label: &str, ty: FieldType, default: Option<&str>) -> FieldSpec {
    FieldSpec {
        group: "TLS".into(),
        ..field(key, label, ty, default, false)
    }
}

fn pem_file(extensions: &[&str]) -> FieldType {
    FieldType::File {
        save: false,
        extensions: extensions.iter().map(|e| e.to_string()).collect(),
    }
}

fn base_manifest() -> DriverManifest {
    let ssl_modes = [
        ("disable", "Disable"),
        ("prefer", "Prefer"),
        ("require", "Require"),
        ("verify-ca", "Verify CA"),
        ("verify-full", "Verify full"),
    ]
    .iter()
    .map(|(v, l)| (v.to_string(), l.to_string()))
    .collect();
    DriverManifest {
        id: "postgres".into(),
        name: "PostgreSQL".into(),
        family: Family::Relational,
        capabilities: Capabilities {
            transactions: true,
            server_cancel: true,
            editable_results: true,
            schemas: true,
            ssh: true,
            dump: true,
            ..Default::default()
        },
        fields: vec![
            field("host", "Host", FieldType::Text, Some("localhost"), true),
            field("port", "Port", FieldType::Port, Some("5432"), true),
            field(
                "database",
                "Database",
                FieldType::Text,
                Some("postgres"),
                true,
            ),
            field(
                "show_all_databases",
                "Show all databases",
                FieldType::Bool,
                Some("false"),
                false,
            ),
            field("user", "User", FieldType::Text, Some("postgres"), true),
            field("password", "Password", FieldType::Password, None, false),
            tls_field(
                "ssl_mode",
                "SSL mode",
                FieldType::Choice(ssl_modes),
                Some("prefer"),
            ),
            tls_field(
                "ssl_root_cert",
                "Root certificate",
                pem_file(&["pem", "crt", "cer"]),
                None,
            ),
            tls_field(
                "ssl_cert",
                "Client certificate",
                pem_file(&["pem", "crt", "cer"]),
                None,
            ),
            tls_field("ssl_key", "Client key", pem_file(&["pem", "key"]), None),
        ],
        dialect: "postgres".into(),
        url_schemes: vec!["postgresql".into(), "postgres".into()],
        default_port: Some(5432),
        editing: Some(editing()),
    }
}

fn editing() -> ObjectEditing {
    use ObjectKind::*;
    let creatable = vec![
        Database,
        Schema,
        Table,
        View,
        MaterializedView,
        Index,
        Sequence,
        Function,
        Procedure,
        Trigger,
    ];
    ObjectEditing {
        alter: vec![Table, View, MaterializedView, Function, Procedure],
        drop: creatable.clone(),
        rename: creatable.clone(),
        create: creatable,
        transactional_ddl: true,
        column_types: [
            "integer",
            "bigint",
            "smallint",
            "numeric",
            "real",
            "double precision",
            "boolean",
            "text",
            "character varying(255)",
            "character(1)",
            "date",
            "time",
            "timestamp",
            "timestamp with time zone",
            "interval",
            "uuid",
            "json",
            "jsonb",
            "bytea",
            "inet",
            "integer[]",
            "text[]",
        ]
        .iter()
        .map(|t| t.to_string())
        .collect(),
        comments: true,
        column_order: false,
    }
}

pub(crate) fn manifest() -> DriverManifest {
    let mut m = base_manifest();
    m.fields
        .extend(sqlr_core::driver_property_fields("postgres"));
    m
}
