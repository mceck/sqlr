//! Static manifest of the MySQL/MariaDB driver.

use sqlr_core::{
    Capabilities, DriverManifest, Family, FieldSpec, FieldType, ObjectEditing, ObjectKind,
};

/// `ssl_mode` values, as in the `mysql` client's `--ssl-mode`.
pub(crate) const SSL_MODES: [(&str, &str); 5] = [
    ("disabled", "Disabled"),
    ("preferred", "Preferred"),
    ("required", "Required"),
    ("verify_ca", "Verify CA"),
    ("verify_identity", "Verify identity"),
];

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

fn pem_file() -> FieldType {
    FieldType::File {
        save: false,
        extensions: vec!["pem".into(), "crt".into(), "key".into()],
    }
}

fn base_manifest() -> DriverManifest {
    let main = "Main";
    let tls = "TLS";
    DriverManifest {
        id: "mysql".into(),
        name: "MySQL / MariaDB".into(),
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
            field(
                "host",
                "Host",
                FieldType::Text,
                Some("localhost"),
                true,
                main,
            ),
            field("port", "Port", FieldType::Port, Some("3306"), true, main),
            field("database", "Database", FieldType::Text, None, false, main),
            field("user", "User", FieldType::Text, Some("root"), true, main),
            field(
                "password",
                "Password",
                FieldType::Password,
                None,
                false,
                main,
            ),
            field(
                "show_all_databases",
                "Show all databases",
                FieldType::Bool,
                Some("false"),
                false,
                main,
            ),
            field(
                "ssl_mode",
                "SSL mode",
                FieldType::Choice(
                    SSL_MODES
                        .iter()
                        .map(|(v, l)| (v.to_string(), l.to_string()))
                        .collect(),
                ),
                Some("preferred"),
                false,
                tls,
            ),
            field("ssl_ca", "CA certificate", pem_file(), None, false, tls),
            field(
                "ssl_cert",
                "Client certificate",
                pem_file(),
                None,
                false,
                tls,
            ),
            field("ssl_key", "Client key", pem_file(), None, false, tls),
        ],
        dialect: "mysql".into(),
        url_schemes: vec!["mysql".into(), "mariadb".into()],
        default_port: Some(3306),
        editing: Some(editing()),
    }
}

/// MySQL databases are the schema level: they are created and dropped as
/// [`ObjectKind::Database`]. DDL is not transactional.
fn editing() -> ObjectEditing {
    use ObjectKind::*;
    let creatable = vec![Database, Table, View, Index, Function, Procedure, Trigger];
    ObjectEditing {
        alter: vec![Database, Table, View, Function, Procedure],
        drop: creatable.clone(),
        rename: vec![Table, View, Index],
        create: creatable,
        transactional_ddl: false,
        column_types: [
            "int",
            "bigint",
            "smallint",
            "tinyint",
            "decimal(10,2)",
            "double",
            "float",
            "varchar(255)",
            "char(1)",
            "text",
            "longtext",
            "date",
            "datetime",
            "timestamp",
            "time",
            "json",
            "blob",
            "varbinary(255)",
            "enum('a','b')",
        ]
        .iter()
        .map(|t| t.to_string())
        .collect(),
        comments: true,
        column_order: true,
    }
}

pub(crate) fn manifest() -> DriverManifest {
    let mut m = base_manifest();
    m.fields.extend(sqlr_core::driver_property_fields("mysql"));
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_fields_and_groups() {
        let m = manifest();
        assert_eq!((m.id.as_str(), m.dialect.as_str()), ("mysql", "mysql"));
        let keys: Vec<&str> = m.fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "host",
                "port",
                "database",
                "user",
                "password",
                "show_all_databases",
                "ssl_mode",
                "ssl_ca",
                "ssl_cert",
                "ssl_key",
                "property.sql_mode",
                "property.time_zone",
                "property.wait_timeout"
            ]
        );
        let password = &m.fields[4];
        assert!(password.secret && !password.required);
        assert!(!m.fields[2].required, "database is optional");
        assert!(m.fields[6..10].iter().all(|f| f.group == "TLS"));
        assert!(m.fields[10..].iter().all(|f| f.group == "Properties"));
        assert!(m.capabilities.transactions && m.capabilities.editable_results);
        assert_eq!(m.default_port, Some(3306));
    }
}
