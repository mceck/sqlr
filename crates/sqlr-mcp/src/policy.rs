//! Classification and safety-floor checks shared by the tools.

use serde_json::{Value, json};
use sqlr_core::{
    CommitMode, ConnectionProfile, DriverManifest, ErrorKind, FieldType, LexState, SettingKind,
    SettingValue, SqlSyntax, StatementClass,
};

use crate::error::{ToolError, ToolResult};

/// Splits SQL into statements, dropping empty and comment-only pieces.
pub fn sql_statements(syntax: &SqlSyntax, text: &str) -> Vec<String> {
    syntax
        .split(text, LexState::default())
        .into_iter()
        .map(|range| text[range].to_string())
        .filter(|statement| !statement.trim().is_empty())
        .collect()
}

pub fn classify_sql(syntax: &SqlSyntax, text: &str) -> StatementClass {
    syntax.classify_script(text)
}

/// Whether a statement needs the person's approval under `profile`.
pub fn approval_needed(profile: &ConnectionProfile, class: &StatementClass) -> bool {
    match class {
        StatementClass::Destructive(_) => profile.confirm_destructive || profile.confirm_writes,
        class if class.modifies() => profile.confirm_writes,
        _ => false,
    }
}

/// How a target profile is looser than the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LooserProfile {
    /// The current profile is read-only and the target is not.
    #[error("the target profile is not read-only")]
    NotReadOnly,
    /// The current profile confirms writes and the target does not.
    #[error("the target profile does not confirm writes")]
    NoWriteConfirmation,
    /// The current profile confirms destructive statements and the target does not.
    #[error("the target profile does not confirm destructive statements")]
    NoDestructiveConfirmation,
    /// The target's idle transaction timeout is longer than the current one, or absent.
    #[error("the target profile's idle transaction timeout is longer or absent")]
    LongerIdleTimeout,
    /// The target commits more eagerly.
    #[error("the target profile commits more eagerly")]
    EagerCommit,
}

/// Whether `target` is at least as strict as `current` on every safety field.
pub fn profile_is_at_least_as_strict(
    current: &ConnectionProfile,
    target: &ConnectionProfile,
) -> Result<(), LooserProfile> {
    if current.read_only && !target.read_only {
        return Err(LooserProfile::NotReadOnly);
    }
    if current.confirm_writes && !target.confirm_writes {
        return Err(LooserProfile::NoWriteConfirmation);
    }
    if current.confirm_destructive && !target.confirm_destructive {
        return Err(LooserProfile::NoDestructiveConfirmation);
    }
    if let Some(current_timeout) = current.idle_tx_timeout_min {
        match target.idle_tx_timeout_min {
            Some(target_timeout) if target_timeout <= current_timeout => {}
            _ => {
                return Err(LooserProfile::LongerIdleTimeout);
            }
        }
    }
    if commit_mode_looser(current.commit_mode, target.commit_mode) {
        return Err(LooserProfile::EagerCommit);
    }
    Ok(())
}

fn commit_mode_looser(current: CommitMode, target: CommitMode) -> bool {
    // Manual is the strictest (nothing commits without the person), then Smart, then Auto.
    let rank = |mode| match mode {
        CommitMode::Manual => 0,
        CommitMode::Smart => 1,
        CommitMode::Auto => 2,
    };
    rank(target) > rank(current)
}

/// Parses a setting value strictly (no clamping) for the setting's kind.
pub fn parse_setting_value(id: &str, kind: SettingKind, value: &Value) -> ToolResult<SettingValue> {
    let wrong = || ToolError::invalid(format!("invalid value for {id}"));
    match kind {
        SettingKind::Bool => value.as_bool().map(SettingValue::Bool).ok_or_else(wrong),
        SettingKind::Int { min, max } => {
            let n = value.as_i64().ok_or_else(wrong)?;
            if (min..=max).contains(&n) {
                Ok(SettingValue::Int(n))
            } else {
                Err(ToolError::invalid(format!(
                    "{id} must be between {min} and {max}"
                )))
            }
        }
        SettingKind::Float { min, max, .. } => {
            let n = value.as_f64().ok_or_else(wrong)?;
            if n.is_finite() && (min..=max).contains(&n) {
                Ok(SettingValue::Float(n))
            } else {
                Err(ToolError::invalid(format!(
                    "{id} must be between {min} and {max}"
                )))
            }
        }
        SettingKind::Text => value
            .as_str()
            .map(|s| SettingValue::Text(s.to_string()))
            .ok_or_else(wrong),
        SettingKind::Path { .. } => value
            .as_str()
            .map(|s| SettingValue::Path(s.to_string()))
            .ok_or_else(wrong),
        SettingKind::Choice(options) => {
            let text = value.as_str().ok_or_else(wrong)?;
            if options.iter().any(|(id, _)| *id == text) {
                Ok(SettingValue::Choice(text.to_string()))
            } else {
                Err(ToolError::invalid(format!(
                    "{text:?} is not an option of {id}"
                )))
            }
        }
        SettingKind::Color => {
            let array = value.as_array().ok_or_else(wrong)?;
            if array.len() != 3 {
                return Err(wrong());
            }
            let channel = |v: &Value| {
                v.as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(wrong)
            };
            Ok(SettingValue::Color([
                channel(&array[0])?,
                channel(&array[1])?,
                channel(&array[2])?,
            ]))
        }
    }
}

/// Whether a profile safety field would be loosened by `new` from `old`.
pub fn tightened_only(name: &str, old: bool, new: bool) -> ToolResult<()> {
    if old && !new {
        Err(ToolError::invalid(format!(
            "{name} is a safety flag and cannot be disabled by an agent"
        )))
    } else {
        Ok(())
    }
}

/// A setting's kind as the contract spells it.
pub fn setting_kind_json(kind: SettingKind) -> Value {
    match kind {
        SettingKind::Bool => json!({"type": "bool"}),
        SettingKind::Int { min, max } => json!({"type": "int", "min": min, "max": max}),
        SettingKind::Float { min, max, step } => {
            json!({"type": "float", "min": min, "max": max, "step": step})
        }
        SettingKind::Text => json!({"type": "text"}),
        SettingKind::Path { dir } => json!({"type": "path", "dir": dir}),
        SettingKind::Choice(options) => json!({
            "type": "choice",
            "options": options.iter().map(|(id, label)| json!({"id": id, "label": label})).collect::<Vec<_>>()
        }),
        SettingKind::Color => json!({"type": "color"}),
    }
}

/// A driver manifest with field metadata but no values.
pub fn manifest_json(manifest: &DriverManifest) -> Value {
    json!({
        "id": manifest.id,
        "name": manifest.name,
        "family": family_name(manifest.family),
        "dialect": manifest.dialect,
        "default_port": manifest.default_port,
        "capabilities": {
            "transactions": manifest.capabilities.transactions,
            "server_cancel": manifest.capabilities.server_cancel,
            "editable_results": manifest.capabilities.editable_results,
            "explain": manifest.capabilities.explain,
            "schemas": manifest.capabilities.schemas,
            "ssh": manifest.capabilities.ssh,
        },
        "fields": manifest.fields.iter().map(|field| json!({
            "key": field.key,
            "label": field.label,
            "type": field_type_json(&field.ty),
            "default": field.default,
            "required": field.required,
            "secret": field.secret,
            "group": field.group,
            "tighten_only": is_transport_security_field(&field.key),
        })).collect::<Vec<_>>(),
    })
}

fn family_name(family: sqlr_core::Family) -> &'static str {
    match family {
        sqlr_core::Family::Relational => "relational",
        sqlr_core::Family::Document => "document",
        sqlr_core::Family::KeyValue => "key_value",
    }
}

fn field_type_json(ty: &FieldType) -> Value {
    match ty {
        FieldType::Text => json!({"kind": "text"}),
        FieldType::Port => json!({"kind": "port"}),
        FieldType::Password => json!({"kind": "password"}),
        FieldType::Bool => json!({"kind": "bool"}),
        FieldType::File { save, extensions } => {
            json!({"kind": "file", "save": save, "extensions": extensions})
        }
        FieldType::Choice(options) => json!({
            "kind": "choice",
            "options": options.iter().map(|(id, label)| json!({"id": id, "label": label})).collect::<Vec<_>>()
        }),
        FieldType::Number => json!({"kind": "number"}),
    }
}

/// Driver fields that weaken transport security. An agent may only tighten them.
pub fn is_transport_security_field(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == "ssl_mode"
        || key.contains("tls_verify")
        || key.contains("verify_cert")
        || key.contains("verify_server")
        || key.contains("check_hostname")
        || key.contains("host_key")
        || key.contains("insecure")
        || key.contains("strict_host")
        || key.contains("validate")
}

/// A boolean security field is tightened by moving to the safe value.
pub fn transport_security_ok(key: &str, old: Option<&str>, new: &str) -> ToolResult<()> {
    if !is_transport_security_field(key) {
        return Ok(());
    }
    if key.eq_ignore_ascii_case("ssl_mode") {
        // PostgreSQL and MySQL order their TLS modes from disabled to certificate checked.
        let rank = |v: &str| match v.trim().to_ascii_lowercase().as_str() {
            "disable" => 0,
            "allow" => 1,
            "prefer" => 2,
            "require" => 3,
            "verify-ca" | "verify_ca" => 4,
            "verify-full" | "verify_full" => 5,
            _ => 0,
        };
        let weakens = old.is_some_and(|old| rank(old) > rank(new));
        return if weakens {
            Err(ToolError::invalid(format!(
                "{key} weakens transport security and can only be tightened"
            )))
        } else {
            Ok(())
        };
    }
    let safe = |v: &str| {
        let v = v.trim().to_ascii_lowercase();
        !matches!(
            v.as_str(),
            "false" | "0" | "no" | "off" | "none" | "skip" | "disabled"
        )
    };
    let old_safe = old.is_some_and(safe);
    let new_safe = safe(new);
    if old_safe && !new_safe {
        Err(ToolError::invalid(format!(
            "{key} weakens transport security and can only be tightened"
        )))
    } else {
        Ok(())
    }
}

/// The class name of a statement for risk labels and logs.
pub fn class_name(class: &StatementClass) -> String {
    match class {
        StatementClass::Read => "read".into(),
        StatementClass::Write => "write".into(),
        StatementClass::Ddl => "ddl".into(),
        StatementClass::Destructive(reason) => format!("destructive: {reason}"),
        StatementClass::Transaction(_) => "transaction".into(),
        StatementClass::Other => "other".into(),
    }
}

pub fn is_read_only_error(err: &sqlr_core::DriverError) -> bool {
    err.kind == ErrorKind::Permission
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::ConnectionProfile;

    fn profile(read_only: bool, writes: bool, destructive: bool) -> ConnectionProfile {
        ConnectionProfile {
            id: "p".into(),
            name: "P".into(),
            color: None,
            commit_mode: CommitMode::Auto,
            confirm_writes: writes,
            confirm_destructive: destructive,
            read_only,
            idle_tx_timeout_min: None,
        }
    }

    #[test]
    fn profiles_can_only_become_stricter() {
        let loose = profile(false, false, true);
        assert!(profile_is_at_least_as_strict(&loose, &profile(false, true, true)).is_ok());
        assert!(profile_is_at_least_as_strict(&loose, &profile(true, true, true)).is_ok());
        let strict = profile(true, true, true);
        assert_eq!(
            profile_is_at_least_as_strict(&strict, &loose)
                .unwrap_err()
                .to_string(),
            "the target profile is not read-only"
        );
    }

    #[test]
    fn setting_values_are_strict() {
        assert!(parse_setting_value("x", SettingKind::Int { min: 1, max: 3 }, &json!(4)).is_err());
        assert_eq!(
            parse_setting_value("x", SettingKind::Int { min: 1, max: 3 }, &json!(2)).unwrap(),
            SettingValue::Int(2)
        );
        assert!(parse_setting_value("x", SettingKind::Bool, &json!("true")).is_err());
        assert!(parse_setting_value("x", SettingKind::Choice(&[("a", "A")]), &json!("b")).is_err());
        assert_eq!(
            parse_setting_value("c", SettingKind::Color, &json!([1, 2, 3])).unwrap(),
            SettingValue::Color([1, 2, 3])
        );
        // Every channel must be a valid byte: an extra element must not be silently dropped.
        for bad in [
            json!([1, 2, 3, 4]),
            json!([1, 2, 3, 256]),
            json!([1, 2, 3, "x"]),
            json!([1, 2]),
        ] {
            assert!(
                parse_setting_value("c", SettingKind::Color, &bad).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn transport_security_only_tightens() {
        assert!(transport_security_ok("tls_verify", Some("false"), "true").is_ok());
        assert!(transport_security_ok("tls_verify", Some("true"), "false").is_err());
        assert!(transport_security_ok("tls_verify", None, "false").is_ok());
        assert!(transport_security_ok("host", Some("x"), "y").is_ok());
        assert!(is_transport_security_field("ssl_mode"));
        assert!(transport_security_ok("ssl_mode", Some("verify-full"), "disable").is_err());
        assert!(transport_security_ok("ssl_mode", Some("require"), "verify-full").is_ok());
        assert!(transport_security_ok("ssl_mode", Some("prefer"), "require").is_ok());
    }

    #[test]
    fn looser_profile_errors_keep_their_messages() {
        let cases = [
            (
                LooserProfile::NotReadOnly,
                "the target profile is not read-only",
            ),
            (
                LooserProfile::NoWriteConfirmation,
                "the target profile does not confirm writes",
            ),
            (
                LooserProfile::NoDestructiveConfirmation,
                "the target profile does not confirm destructive statements",
            ),
            (
                LooserProfile::LongerIdleTimeout,
                "the target profile's idle transaction timeout is longer or absent",
            ),
            (
                LooserProfile::EagerCommit,
                "the target profile commits more eagerly",
            ),
        ];
        for (error, message) in cases {
            assert_eq!(error.to_string(), message);
        }
    }
}
