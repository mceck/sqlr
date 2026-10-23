//! Static manifest of the Oracle driver.

use sqlr_core::{Capabilities, DriverManifest, Family, FieldSpec, FieldType};

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
    let main = "Main";
    let tls = "TLS";
    DriverManifest {
        id: "oracle".into(),
        name: "Oracle".into(),
        family: Family::Relational,
        capabilities: Capabilities {
            transactions: true,
            // oracle-rs has no out-of-band cancel: a running statement must finish or the
            // connection must be closed.
            server_cancel: false,
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
            field("port", "Port", FieldType::Port, Some("1521"), true, main),
            field(
                "service",
                "Service name",
                FieldType::Text,
                Some("FREEPDB1"),
                true,
                main,
            ),
            field(
                "service_type",
                "Connect by",
                FieldType::Choice(
                    [("service", "Service name"), ("sid", "SID")]
                        .iter()
                        .map(|(v, l)| (v.to_string(), l.to_string()))
                        .collect(),
                ),
                Some("service"),
                false,
                main,
            ),
            field("user", "User", FieldType::Text, Some("system"), true, main),
            field(
                "password",
                "Password",
                FieldType::Password,
                None,
                false,
                main,
            ),
            field("tls", "TLS", FieldType::Bool, Some("false"), false, tls),
        ],
        dialect: "oracle".into(),
        url_schemes: vec!["oracle".into()],
        default_port: Some(1521),
        editing: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_identity_fields_and_capabilities() {
        let m = manifest();
        assert_eq!((m.id.as_str(), m.dialect.as_str()), ("oracle", "oracle"));
        assert_eq!(m.family, Family::Relational);
        assert_eq!(m.default_port, Some(1521));
        let keys: Vec<&str> = m.fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "host",
                "port",
                "service",
                "service_type",
                "user",
                "password",
                "tls"
            ]
        );
        assert!(m.fields[5].secret);
        assert!(m.capabilities.transactions);
        assert!(!m.capabilities.server_cancel);
        assert!(m.url_schemes.contains(&"oracle".to_string()));
    }
}
