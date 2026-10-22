//! Static manifest of the Elasticsearch driver.

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
    let (main, tls, adv) = ("Main", "TLS", "Advanced");
    DriverManifest {
        id: "elasticsearch".into(),
        name: "Elasticsearch".into(),
        family: Family::Document,
        capabilities: Capabilities {
            server_cancel: true,
            editable_results: true,
            tabular_query: true,
            ssh: true,
            ..Default::default()
        },
        fields: vec![
            field(
                "scheme",
                "Protocol",
                FieldType::Choice(vec![
                    ("http".into(), "HTTP".into()),
                    ("https".into(), "HTTPS".into()),
                ]),
                Some("http"),
                true,
                main,
            ),
            field(
                "host",
                "Host",
                FieldType::Text,
                Some("localhost"),
                true,
                main,
            ),
            field("port", "Port", FieldType::Port, Some("9200"), true, main),
            field("user", "User", FieldType::Text, None, false, main),
            field(
                "password",
                "Password",
                FieldType::Password,
                None,
                false,
                main,
            ),
            field("api_key", "API key", FieldType::Password, None, false, main),
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
                "tls_skip_verify",
                "Skip certificate verification",
                FieldType::Bool,
                Some("false"),
                false,
                tls,
            ),
            field(
                "pit_keep_alive",
                "Point-in-time keep-alive",
                FieldType::Text,
                Some("5m"),
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
        dialect: "elasticsearch".into(),
        url_schemes: vec!["elasticsearch".into()],
        default_port: Some(9200),
        editing: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_fields() {
        let m = manifest();
        assert_eq!(m.language(), sqlr_core::QueryLanguage::EsConsole);
        let secret: Vec<&str> = m
            .fields
            .iter()
            .filter(|f| f.secret)
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(secret, ["password", "api_key"]);
        assert!(m.capabilities.tabular_query);
    }
}
