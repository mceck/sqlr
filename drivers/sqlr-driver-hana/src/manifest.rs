//! The connection form and capabilities.

use sqlr_driver_sdk::{Capabilities, DriverManifest, Family, FieldSpec, FieldType};

fn field(
    key: &str,
    label: &str,
    ty: FieldType,
    default: Option<&str>,
    required: bool,
    secret: bool,
    group: &str,
) -> FieldSpec {
    FieldSpec {
        free_form: false,
        key: key.into(),
        label: label.into(),
        ty,
        default: default.map(str::to_string),
        required,
        secret,
        group: group.into(),
    }
}

pub(crate) fn manifest() -> DriverManifest {
    DriverManifest {
        id: "hana".into(),
        name: "SAP HANA".into(),
        family: Family::Relational,
        capabilities: Capabilities {
            server_cancel: true,
            editable_results: true,
            schemas: true,
            ..Capabilities::default()
        },
        fields: vec![
            field("host", "Host", FieldType::Text, None, true, false, "Main"),
            // 3<instance>15 for a tenant database, 3<instance>13 for SYSTEMDB (HANA Cloud: 443).
            field(
                "port",
                "Port",
                FieldType::Port,
                Some("30015"),
                true,
                false,
                "Main",
            ),
            field(
                "database",
                "Tenant database",
                FieldType::Text,
                None,
                false,
                false,
                "Main",
            ),
            field("user", "User", FieldType::Text, None, true, false, "Main"),
            field(
                "password",
                "Password",
                FieldType::Password,
                None,
                false,
                true,
                "Main",
            ),
            field(
                "schema",
                "Default schema",
                FieldType::Text,
                None,
                false,
                false,
                "Main",
            ),
            field(
                "tls",
                "Use TLS",
                FieldType::Bool,
                Some("true"),
                false,
                false,
                "TLS",
            ),
            field(
                "tls_ca_file",
                "CA certificate (PEM)",
                FieldType::File {
                    save: false,
                    extensions: vec!["pem".into(), "crt".into(), "cer".into()],
                },
                None,
                false,
                false,
                "TLS",
            ),
            field(
                "tls_skip_verify",
                "Skip certificate verification",
                FieldType::Bool,
                Some("false"),
                false,
                false,
                "TLS",
            ),
            field(
                "pin_redirect",
                "Pin the redirect to this host",
                FieldType::Bool,
                Some("false"),
                false,
                false,
                "Advanced",
            ),
            field(
                "tenant_port",
                "Tenant SQL port (for the pin)",
                FieldType::Port,
                None,
                false,
                false,
                "Advanced",
            ),
        ],
        dialect: "hana".into(),
        url_schemes: vec!["hana".into(), "saphana".into()],
        default_port: Some(30015),
        editing: None,
    }
}
