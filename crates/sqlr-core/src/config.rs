//! Saved connection configuration and in-memory secrets.

use std::collections::BTreeMap;
use std::fmt;

use crate::manifest::DriverManifest;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConnectionConfig {
    pub id: String,
    pub name: String,
    pub driver: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
    /// The connection profile's id; `None` means the built-in `development` profile.
    #[serde(default)]
    pub profile: Option<String>,
    /// Secret fields are kept in the keychain; when `false` they are asked for on connect
    /// and kept in memory only.
    #[serde(default = "yes")]
    pub save_secrets: bool,
}

fn yes() -> bool {
    true
}

/// The `values` key the engine sets when the driver dials a different address than the
/// host the user named (an SSH tunnel's `127.0.0.1`): drivers use it as the TLS server
/// name (SNI and certificate host check).
pub const TLS_SERVER_NAME: &str = "tls_server_name";

impl ConnectionConfig {
    /// The host name TLS must verify when it differs from the address dialled
    /// ([`TLS_SERVER_NAME`]).
    pub fn tls_server_name(&self) -> Option<&str> {
        self.values
            .get(TLS_SERVER_NAME)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }

    /// The stored value, else the manifest field's default.
    pub fn value<'a>(&'a self, manifest: &'a DriverManifest, key: &str) -> Option<&'a str> {
        self.values.get(key).map(String::as_str).or_else(|| {
            manifest
                .fields
                .iter()
                .find(|f| f.key == key)
                .and_then(|f| f.default.as_deref())
        })
    }
}

/// Secret values by field key. `Debug` prints keys only.
#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Secrets(pub BTreeMap<String, String>);

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

/// The UI theme the user picked; `System` follows the OS light/dark setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ThemePref {
    #[default]
    System,
    Dark,
    Light,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Capabilities, Family, FieldSpec, FieldType};

    #[test]
    fn value_falls_back_to_default() {
        let manifest = DriverManifest {
            id: "x".into(),
            name: "X".into(),
            family: Family::Relational,
            capabilities: Capabilities::default(),
            fields: vec![FieldSpec {
                free_form: false,
                key: "port".into(),
                label: "Port".into(),
                ty: FieldType::Port,
                default: Some("5432".into()),
                required: true,
                secret: false,
                group: "Main".into(),
            }],
            dialect: "x".into(),
            url_schemes: Vec::new(),
            default_port: None,
            editing: None,
        };
        let mut cfg = ConnectionConfig {
            id: "1".into(),
            name: "n".into(),
            driver: "x".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: true,
        };
        assert_eq!(cfg.value(&manifest, "port"), Some("5432"));
        cfg.values.insert("port".into(), "6543".into());
        assert_eq!(cfg.value(&manifest, "port"), Some("6543"));
        assert_eq!(cfg.value(&manifest, "nope"), None);
    }

    #[test]
    fn profile_and_save_secrets_default_when_missing() {
        let cfg: ConnectionConfig =
            serde_json::from_str(r#"{"id":"1","name":"n","driver":"x"}"#).unwrap();
        assert_eq!(cfg.profile, None);
        assert!(cfg.save_secrets);
    }

    #[test]
    fn secrets_debug_hides_values() {
        let s = Secrets([("password".to_string(), "hunter2".to_string())].into());
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("hunter2"));
        assert!(dbg.contains("password"));
    }
}
