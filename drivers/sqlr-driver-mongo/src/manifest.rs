//! Static manifest of the MongoDB driver.

use sqlr_core::{Capabilities, DriverManifest, Family, FieldSpec, FieldType};

/// `tls` values.
pub(crate) const TLS_MODES: [(&str, &str); 3] = [
    ("disabled", "Disabled"),
    ("required", "Required (verify certificate)"),
    ("allow_invalid", "Required (skip verification)"),
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

pub(crate) fn manifest() -> DriverManifest {
    let (main, tls, adv) = ("Main", "TLS", "Advanced");
    DriverManifest {
        id: "mongodb".into(),
        name: "MongoDB".into(),
        family: Family::Document,
        capabilities: Capabilities {
            server_cancel: true,
            editable_results: true,
            ssh: true,
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
            field("port", "Port", FieldType::Port, Some("27017"), true, main),
            field(
                "database",
                "Database",
                FieldType::Text,
                Some("test"),
                false,
                main,
            ),
            field("user", "User", FieldType::Text, None, false, main),
            field(
                "password",
                "Password",
                FieldType::Password,
                None,
                false,
                main,
            ),
            field(
                "tls",
                "TLS",
                FieldType::Choice(
                    TLS_MODES
                        .iter()
                        .map(|(v, l)| (v.to_string(), l.to_string()))
                        .collect(),
                ),
                Some("disabled"),
                false,
                tls,
            ),
            field(
                "tls_ca",
                "CA certificate",
                FieldType::File {
                    save: false,
                    extensions: vec!["pem".into(), "crt".into()],
                },
                None,
                false,
                tls,
            ),
            field(
                "auth_source",
                "Auth database",
                FieldType::Text,
                None,
                false,
                adv,
            ),
            field(
                "replica_set",
                "Replica set",
                FieldType::Text,
                None,
                false,
                adv,
            ),
            field(
                "direct_connection",
                "Direct connection",
                FieldType::Bool,
                Some("true"),
                false,
                adv,
            ),
            field(
                "read_only",
                "Read-only",
                FieldType::Bool,
                Some("false"),
                false,
                adv,
            ),
        ],
        dialect: "mongodb".into(),
        url_schemes: vec!["mongodb".into()],
        default_port: Some(27017),
        editing: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_fields() {
        let m = manifest();
        assert_eq!(m.family, Family::Document);
        assert_eq!(m.language(), sqlr_core::QueryLanguage::MongoShell);
        let secret: Vec<&str> = m
            .fields
            .iter()
            .filter(|f| f.secret)
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(secret, ["password"]);
        assert!(m.capabilities.server_cancel && m.capabilities.editable_results);
    }
}
