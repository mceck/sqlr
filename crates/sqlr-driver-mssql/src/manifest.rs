//! Static manifest of the SQL Server driver.

use sqlr_core::{
    Capabilities, DriverManifest, Family, FieldSpec, FieldType, ObjectEditing, ObjectKind,
};

fn field(
    key: &str,
    label: &str,
    ty: FieldType,
    default: Option<&str>,
    required: bool,
    group: &str,
) -> FieldSpec {
    FieldSpec {
        free_form: false,
        key: key.into(),
        label: label.into(),
        secret: ty == FieldType::Password,
        ty,
        default: default.map(String::from),
        required,
        group: group.into(),
    }
}

fn encryption_choices() -> FieldType {
    FieldType::Choice(
        [
            ("required", "Required"),
            ("on", "On when possible"),
            ("off", "Login only"),
            ("not_supported", "Off"),
        ]
        .iter()
        .map(|(v, l)| (v.to_string(), l.to_string()))
        .collect(),
    )
}

fn editing() -> ObjectEditing {
    use ObjectKind::*;
    let creatable = vec![Schema, Table, View, Index];
    ObjectEditing {
        alter: vec![Table, View],
        drop: creatable.clone(),
        rename: vec![Table, View, Index],
        create: creatable,
        transactional_ddl: true,
        column_types: [
            "int",
            "bigint",
            "smallint",
            "tinyint",
            "bit",
            "decimal(18,2)",
            "float",
            "real",
            "money",
            "nvarchar(255)",
            "nvarchar(max)",
            "varchar(255)",
            "varchar(max)",
            "char(10)",
            "date",
            "datetime2",
            "datetimeoffset",
            "time",
            "uniqueidentifier",
            "varbinary(max)",
            "xml",
        ]
        .iter()
        .map(|t| t.to_string())
        .collect(),
        comments: false,
        column_order: false,
    }
}

pub(crate) fn manifest() -> DriverManifest {
    let main = "Main";
    let tls = "TLS";
    DriverManifest {
        id: "sqlserver".into(),
        name: "SQL Server".into(),
        family: Family::Relational,
        capabilities: Capabilities {
            explain: true,
            transactions: true,
            server_cancel: true,
            editable_results: true,
            schemas: true,
            ssh: true,
            tabular_query: true,
            ..Default::default()
        },
        fields: vec![
            field(
                "host",
                "Host",
                FieldType::Text,
                Some("localhost"),
                true,
                main,
            ),
            field("port", "Port", FieldType::Port, Some("1433"), true, main),
            field(
                "database",
                "Database",
                FieldType::Text,
                Some("master"),
                true,
                main,
            ),
            field("user", "User", FieldType::Text, Some("sa"), true, main),
            field(
                "password",
                "Password",
                FieldType::Password,
                None,
                false,
                main,
            ),
            field(
                "application_name",
                "Application name",
                FieldType::Text,
                Some("sqlr"),
                false,
                main,
            ),
            field(
                "encryption",
                "Encryption",
                encryption_choices(),
                Some("required"),
                false,
                tls,
            ),
            field(
                "trust_server_certificate",
                "Trust server certificate",
                FieldType::Bool,
                Some("false"),
                false,
                tls,
            ),
        ],
        dialect: "sqlserver".into(),
        url_schemes: vec!["sqlserver".into(), "mssql".into()],
        default_port: Some(1433),
        editing: Some(editing()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_identity_fields_and_capabilities() {
        let m = manifest();
        assert_eq!(
            (m.id.as_str(), m.dialect.as_str()),
            ("sqlserver", "sqlserver")
        );
        assert_eq!(m.family, Family::Relational);
        assert_eq!(m.default_port, Some(1433));
        let keys: Vec<&str> = m.fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "host",
                "port",
                "database",
                "user",
                "password",
                "application_name",
                "encryption",
                "trust_server_certificate"
            ]
        );
        assert!(m.fields[4].secret);
        assert!(m.capabilities.transactions);
        assert!(m.capabilities.server_cancel);
        assert!(m.capabilities.editable_results);
        assert!(m.url_schemes.contains(&"mssql".to_string()));
    }
}
