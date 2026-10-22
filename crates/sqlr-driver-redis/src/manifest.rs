//! Static manifest of the Redis driver.

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
        id: "redis".into(),
        name: "Redis".into(),
        family: Family::KeyValue,
        capabilities: Capabilities {
            server_cancel: true,
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
            field("port", "Port", FieldType::Port, Some("6379"), true, main),
            field(
                "database",
                "Database",
                FieldType::Number,
                Some("0"),
                false,
                main,
            ),
            field("user", "User (ACL)", FieldType::Text, None, false, main),
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
                "separator",
                "Key separator",
                FieldType::Text,
                Some(":"),
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
        dialect: "redis".into(),
        url_schemes: vec!["redis".into(), "rediss".into()],
        default_port: Some(6379),
        editing: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_fields() {
        let m = manifest();
        assert_eq!(m.family, Family::KeyValue);
        assert_eq!(m.language(), sqlr_core::QueryLanguage::Redis);
        let secret: Vec<&str> = m
            .fields
            .iter()
            .filter(|f| f.secret)
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(secret, ["password"]);
        assert!(m.capabilities.server_cancel && !m.capabilities.transactions);
        let separator = m.fields.iter().find(|f| f.key == "separator").unwrap();
        assert_eq!(separator.default.as_deref(), Some(":"));
    }
}
