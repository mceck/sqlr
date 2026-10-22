//! Typed, non-secret session properties advertised by built-in driver manifests.
use crate::{DriverError, ErrorKind, FieldSpec, FieldType};
use std::collections::BTreeMap;
pub fn driver_property_fields(driver: &str) -> Vec<FieldSpec> {
    let properties: Vec<(&str, FieldType)> = match driver {
        "postgres" => vec![
            ("application_name", FieldType::Text),
            ("options", FieldType::Text),
            ("statement_timeout", FieldType::Number),
            ("search_path", FieldType::Text),
        ],
        "mysql" => vec![
            ("sql_mode", FieldType::Text),
            ("time_zone", FieldType::Text),
            ("wait_timeout", FieldType::Number),
        ],
        "sqlite" => vec![
            ("foreign_keys", FieldType::Bool),
            (
                "journal_mode",
                FieldType::Choice(
                    ["DELETE", "TRUNCATE", "PERSIST", "MEMORY", "WAL", "OFF"]
                        .iter()
                        .map(|s| (s.to_string(), s.to_string()))
                        .collect(),
                ),
            ),
            ("busy_timeout", FieldType::Number),
            ("cache_size", FieldType::Text),
        ],
        _ => vec![],
    };
    properties
        .into_iter()
        .map(|(name, ty)| FieldSpec {
            free_form: matches!(driver, "postgres" | "mysql"),
            key: format!("property.{name}"),
            label: name.into(),
            ty,
            default: None,
            required: false,
            secret: false,
            group: "Properties".into(),
        })
        .collect()
}
pub fn validate_driver_properties(
    fields: &[FieldSpec],
    values: &BTreeMap<String, String>,
) -> Result<(), DriverError> {
    for (key, value) in values.iter().filter(|(k, _)| k.starts_with("property.")) {
        let name = key.strip_prefix("property.").unwrap_or_default();
        let valid_name = !name.is_empty()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.')
            && name.as_bytes()[0].is_ascii_alphabetic();
        if !valid_name || value.contains('\0') {
            return Err(DriverError::new(
                ErrorKind::Other,
                format!("Invalid driver property {key}"),
            ));
        }
        let field = fields
            .iter()
            .find(|f| f.key == *key && f.group == "Properties");
        let Some(field) = field else {
            if fields
                .iter()
                .any(|f| f.group == "Properties" && f.free_form)
            {
                continue;
            }
            return Err(DriverError::new(
                ErrorKind::Other,
                format!("Unknown driver property {key}"),
            ));
        };
        let valid = match &field.ty {
            FieldType::Number => value.parse::<u64>().is_ok(),
            FieldType::Bool => matches!(value.as_str(), "true" | "false"),
            FieldType::Choice(options) => options.iter().any(|(v, _)| v == value),
            _ => !value.contains('\0'),
        };
        if !valid || key == "property.cache_size" && value.parse::<i64>().is_err() {
            return Err(DriverError::new(
                ErrorKind::Other,
                format!("Invalid driver property {key}"),
            ));
        }
    }
    Ok(())
}
