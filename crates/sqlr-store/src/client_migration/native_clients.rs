//! Native JSON adapters. Schemas are documented in docs/specs/connections.md.
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use sqlr_core::{ConnectionConfig, Secrets};

use super::{
    ClientFormat, ExportDocument, ExportError, ImportBatch, ImportError, blank, driver, portable,
    supports_export,
};

type Credentials = BTreeMap<String, Secrets>;

fn text(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}
fn val<'a>(c: &'a ConnectionConfig, key: &str) -> &'a str {
    c.values.get(key).map(String::as_str).unwrap_or_default()
}
fn copy(c: &mut ConnectionConfig, v: &Value, fields: &[(&str, &str)]) {
    for (from, to) in fields {
        let value = text(v, from);
        if !value.is_empty() {
            c.values.insert((*to).into(), value);
        }
    }
}
fn secret(secrets: &mut Secrets, v: &Value, fields: &[(&str, &str)]) {
    for (from, to) in fields {
        let value = text(v, from);
        if !value.is_empty() {
            secrets.0.insert((*to).into(), value);
        }
    }
}
fn group_path(groups: &[Value], id: &str) -> Result<Option<String>, ImportError> {
    let mut current = id.to_string();
    let mut visited = BTreeSet::new();
    let mut segments = Vec::new();
    while !current.is_empty() {
        if !visited.insert(current.clone()) {
            return Err(ImportError::GroupCycle);
        }
        let Some(group) = groups.iter().find(|g| text(g, "id") == current) else {
            break;
        };
        segments.push(text(group, "name"));
        current = text(group, "parent_id");
    }
    segments.reverse();
    Ok((!segments.is_empty()).then(|| segments.join("/")))
}

pub(super) fn decode(format: ClientFormat, bytes: &[u8]) -> Result<ImportBatch, ImportError> {
    let v: Value = serde_json::from_slice(bytes).map_err(|_| ImportError::InvalidJson)?;
    let tab = format == ClientFormat::Tabularis;
    if tab && v.get("version").is_some_and(|v| v != 1) {
        return Err(ImportError::UnsupportedTabularis);
    }
    let rows = if tab {
        v["connections"]
            .as_array()
            .cloned()
            .ok_or(ImportError::InvalidTabularis)?
    } else if let Some(rows) = v.as_array() {
        rows.clone()
    } else {
        vec![v.clone()]
    };
    let mut batch = ImportBatch::default();
    let groups = v["groups"].as_array().cloned().unwrap_or_default();
    for mut row in rows {
        if !tab && let Some(object) = row.as_object_mut() {
            let snake_fields: Vec<_> = object
                .iter()
                .filter(|(k, _)| k.contains('_'))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (key, value) in snake_fields {
                let mut parts = key.split('_');
                let mut camel = parts.next().unwrap_or_default().to_string();
                for part in parts {
                    let mut chars = part.chars();
                    if let Some(first) = chars.next() {
                        camel.extend(first.to_uppercase());
                        camel.extend(chars);
                    }
                }
                object.entry(camel).or_insert(value);
            }
        }
        let p = if tab { &row["params"] } else { &row };
        let driver_name = text(p, if tab { "driver" } else { "connectionType" });
        let Some(d) = driver(&driver_name) else {
            batch
                .warnings
                .push("A connection with an unsupported driver was excluded.".into());
            continue;
        };
        if tab && (p["k8s_enabled"] == true || p["ssm_enabled"] == true) {
            batch.warnings.push(
                "A connection requiring Kubernetes or AWS SSM tunneling was excluded.".into(),
            );
            continue;
        }
        if !tab && p["sshEnabled"] == true && !text(p, "sshBastionHost").is_empty() {
            batch.warnings.push("A Beekeeper connection requiring an SSH bastion was excluded; SSH jump hosts are not supported by this adapter.".into());
            continue;
        }
        let mut c = blank(&text(&row, "name"), d);
        let mut secrets = Secrets::default();
        copy(
            &mut c,
            p,
            &[("host", "host"), ("port", "port"), ("username", "user")],
        );
        if tab {
            let database = &p["database"];
            let db = if let Some(dbs) = database.as_array() {
                if dbs.len() > 1 {
                    batch.warnings.push("Only the primary database is imported from multi-database Tabularis connections.".into());
                }
                dbs.first().and_then(Value::as_str).unwrap_or_default()
            } else {
                database.as_str().unwrap_or_default()
            };
            c.values.insert(
                if d == "sqlite" { "path" } else { "database" }.into(),
                db.into(),
            );
            copy(
                &mut c,
                p,
                &[
                    ("ssl_mode", "ssl_mode"),
                    (
                        "ssl_ca",
                        if d == "postgres" {
                            "ssl_root_cert"
                        } else {
                            "ssl_ca"
                        },
                    ),
                    ("ssl_cert", "ssl_cert"),
                    ("ssl_key", "ssl_key"),
                    ("ssh_enabled", "ssh_enabled"),
                    ("ssh_host", "ssh_host"),
                    ("ssh_port", "ssh_port"),
                    ("ssh_user", "ssh_user"),
                    ("ssh_key_file", "ssh_key_path"),
                ],
            );
            secret(
                &mut secrets,
                p,
                &[
                    ("password", "password"),
                    ("ssh_password", "ssh_password"),
                    ("ssh_key_passphrase", "ssh_key_passphrase"),
                ],
            );
            c.group = group_path(&groups, &text(&row, "group_id"))?;
            c.profile = match text(&row, "environment").as_str() {
                "production" => Some("production".into()),
                "staging" => Some("test".into()),
                _ => None,
            };
            if let Some(color) = row["appearance"]["accent_color"]
                .as_str()
                .and_then(|s| u32::from_str_radix(s.trim_start_matches('#'), 16).ok())
            {
                c.color = Some([(color >> 16) as u8, (color >> 8) as u8, color as u8]);
            }
            let ssh_id = text(p, "ssh_connection_id");
            if !ssh_id.is_empty() && p["ssh_enabled"] == true {
                let ssh = v["ssh_connections"]
                    .as_array()
                    .and_then(|rows| rows.iter().find(|r| text(r, "id") == ssh_id));
                let Some(ssh) = ssh else {
                    batch.warnings.push("A connection with a missing linked SSH profile was excluded. Export it from Tabularis with SSH profiles included.".into());
                    continue;
                };
                copy(
                    &mut c,
                    ssh,
                    &[
                        ("host", "ssh_host"),
                        ("port", "ssh_port"),
                        ("user", "ssh_user"),
                        ("key_file", "ssh_key_path"),
                    ],
                );
                c.values.insert(
                    "ssh_auth".into(),
                    if ssh["auth_type"] == "ssh_key" {
                        "key"
                    } else {
                        "password"
                    }
                    .into(),
                );
                secret(
                    &mut secrets,
                    ssh,
                    &[
                        ("password", "ssh_password"),
                        ("key_passphrase", "ssh_key_passphrase"),
                    ],
                );
            } else if p["ssh_enabled"] == true {
                c.values.insert(
                    "ssh_auth".into(),
                    if c.values.contains_key("ssh_key_path") {
                        "key"
                    } else {
                        "password"
                    }
                    .into(),
                );
            }
        } else {
            copy(
                &mut c,
                p,
                &[
                    (
                        "defaultDatabase",
                        if d == "sqlite" { "path" } else { "database" },
                    ),
                    ("sshEnabled", "ssh_enabled"),
                    ("sshHost", "ssh_host"),
                    ("sshPort", "ssh_port"),
                    ("sshUsername", "ssh_user"),
                    ("sshKeyfile", "ssh_key_path"),
                    (
                        "sslCaFile",
                        if d == "postgres" {
                            "ssl_root_cert"
                        } else {
                            "ssl_ca"
                        },
                    ),
                    ("sslCertFile", "ssl_cert"),
                    ("sslKeyFile", "ssl_key"),
                ],
            );
            if p["sshEnabled"] == true {
                c.values.insert(
                    "ssh_auth".into(),
                    match p["sshMode"].as_str() {
                        Some("keyfile") => "key",
                        Some("agent") | None => "agent",
                        _ => "password",
                    }
                    .into(),
                );
            }
            let mode = if p["ssl"] == true {
                if p["sslRejectUnauthorized"] == false {
                    "require"
                } else {
                    "verify-full"
                }
            } else {
                "disable"
            };
            c.values.insert("ssl_mode".into(), mode.into());
            secret(
                &mut secrets,
                p,
                &[
                    ("password", "password"),
                    ("sshPassword", "ssh_password"),
                    ("sshKeyfilePassword", "ssh_key_passphrase"),
                ],
            );
        }
        if d == "mysql" {
            let mode = match val(&c, "ssl_mode") {
                "disable" => "disabled",
                "require" => "required",
                "verify-full" => "verify_identity",
                "verify-ca" => "verify_ca",
                mode => mode,
            };
            c.values.insert("ssl_mode".into(), mode.to_string());
        }
        let index = batch.connections.len();
        batch.connections.push(portable(c));
        if !secrets.0.is_empty() {
            batch.credentials.insert(index, secrets);
        }
    }
    if batch.connections.is_empty() {
        return Err(ImportError::NoSupportedConnections);
    }
    Ok(batch)
}

pub(super) fn encode(
    format: ClientFormat,
    connections: &[ConnectionConfig],
    credentials: Option<&Credentials>,
) -> Result<ExportDocument, ExportError> {
    let tab = format == ClientFormat::Tabularis;
    let mut rows = Vec::new();
    let mut files = Vec::new();
    let mut groups = BTreeMap::new();
    let mut ssh_profiles = Vec::new();
    let mut warnings = Vec::new();
    if credentials.is_some() {
        warnings.push("This export contains plaintext credentials. Store it securely.".into());
    }
    for original in connections {
        if !supports_export(format, original) {
            warnings.push(format!(
                "{}: unsupported driver or SSH configuration; excluded.",
                original.name
            ));
            continue;
        }
        if original
            .values
            .get("ssh_jump_host")
            .is_some_and(|s| !s.is_empty())
        {
            warnings.push(format!(
                "{}: SSH jump hosts are not supported by this format; excluded.",
                original.name
            ));
            continue;
        }
        if tab && val(original, "ssh_enabled") == "true" && val(original, "ssh_auth") == "agent" {
            warnings.push(format!(
                "{}: Tabularis cannot represent SSH agent authentication; excluded.",
                original.name
            ));
            continue;
        }
        let c = portable(original.clone());
        let password = |key: &str| {
            credentials
                .and_then(|all| all.get(&c.id))
                .and_then(|s| s.0.get(key))
        };
        let port = val(&c, "port")
            .parse::<u16>()
            .unwrap_or(if c.driver == "mysql" { 3306 } else { 5432 });
        let db = val(
            &c,
            if c.driver == "sqlite" {
                "path"
            } else {
                "database"
            },
        );
        let ssh = val(&c, "ssh_enabled") == "true";
        let ca = val(
            &c,
            if c.driver == "postgres" {
                "ssl_root_cert"
            } else {
                "ssl_ca"
            },
        );
        let mut row = if tab {
            if let Some(group) = &c.group {
                let mut parent: Option<String> = None;
                let mut path = String::new();
                for segment in group.split('/') {
                    if !path.is_empty() {
                        path.push('/');
                    }
                    path.push_str(segment);
                    groups.entry(path.clone()).or_insert_with(|| json!({"id":path,"name":segment,"parent_id":parent,"collapsed":false,"sort_order":0}));
                    parent = Some(path.clone());
                }
            }
            let mut params = json!({"driver":c.driver,"host":val(&c,"host"),"port":port,"username":val(&c,"user"),"database":db,"save_in_keychain":true,"ssl_mode":val(&c,"ssl_mode"),"ssl_ca":ca,"ssl_cert":val(&c,"ssl_cert"),"ssl_key":val(&c,"ssl_key"),"ssh_enabled":ssh});
            if ssh {
                let id = format!("ssh-{}", rows.len());
                params["ssh_connection_id"] = json!(id);
                let mut profile = json!({"id":id,"name":c.name,"host":val(&c,"ssh_host"),"port":val(&c,"ssh_port").parse::<u16>().unwrap_or(22),"user":val(&c,"ssh_user"),"auth_type":if val(&c,"ssh_auth")=="key" {"ssh_key"}else{"password"},"key_file":val(&c,"ssh_key_path"),"save_in_keychain":true});
                for (from, to) in [
                    ("ssh_password", "password"),
                    ("ssh_key_passphrase", "key_passphrase"),
                ] {
                    if let Some(secret) = password(from) {
                        profile[to] = json!(secret);
                    }
                }
                ssh_profiles.push(profile);
            }
            if let Some(secret) = password("password") {
                params["password"] = json!(secret);
            }
            json!({"id":c.id,"name":c.name,"params":params,"group_id":c.group,"environment":match c.profile.as_deref() { Some("production")=>Some("production"),Some("test")=>Some("staging"),_=>None },"appearance":c.color.map(|[r,g,b]|json!({"accent_color":format!("#{r:02X}{g:02X}{b:02X}")}))})
        } else {
            let mode = val(&c, "ssl_mode");
            if matches!(mode, "prefer" | "preferred" | "verify-ca" | "verify_ca") {
                warnings.push(format!(
                    "{}: review TLS settings; Beekeeper uses a different TLS mode model.",
                    c.name
                ));
            }
            let mut row = json!({"name":c.name,"connectionType":match c.driver.as_str(){"postgres"=>"postgresql",d=>d},"host":val(&c,"host"),"port":port,"username":val(&c,"user"),"defaultDatabase":db,"ssl":!mode.is_empty()&&!matches!(mode,"disable"|"disabled"),"sslRejectUnauthorized":matches!(mode,"verify-full"|"verify_identity"|"verify-ca"|"verify_ca"),"sslCaFile":ca,"sslCertFile":val(&c,"ssl_cert"),"sslKeyFile":val(&c,"ssl_key"),"sshEnabled":ssh,"sshHost":val(&c,"ssh_host"),"sshPort":val(&c,"ssh_port").parse::<u16>().unwrap_or(22),"sshUsername":val(&c,"ssh_user"),"sshMode":match val(&c,"ssh_auth"){"key"=>"keyfile","agent"=>"agent",_=>"userpass"},"sshKeyfile":val(&c,"ssh_key_path")});
            for (from, to) in [
                ("password", "password"),
                ("ssh_password", "sshPassword"),
                ("ssh_key_passphrase", "sshKeyfilePassword"),
            ] {
                if let Some(secret) = password(from)
                    && (from == "password" || ssh)
                {
                    row[to] = json!(secret);
                }
            }
            if c.group.is_some() {
                warnings.push(format!(
                    "{}: folder placement is selected when importing into Beekeeper Studio.",
                    c.name
                ));
            }
            row
        };
        // Foreign clients must never inherit sqlr agent consent or arbitrary values.
        if !tab {
            files.push((
                format!("connection-{}.json", rows.len() + 1),
                serde_json::to_string_pretty(&row).map_err(|_| ExportError::SerializeConnection)?,
            ));
        }
        rows.push(row.take());
    }
    if rows.is_empty() {
        return Err(ExportError::NothingExportableHere);
    }
    let value = if tab {
        json!({"version":1,"connections":rows,"groups":groups.into_values().collect::<Vec<_>>(),"ssh_connections":ssh_profiles,"k8s_connections":[],"tags":[]})
    } else if rows.len() == 1 {
        rows.remove(0)
    } else {
        json!(rows)
    };
    Ok(ExportDocument {
        contents: serde_json::to_string_pretty(&value)
            .map_err(|_| ExportError::SerializeConnections)?,
        warnings,
        files,
    })
}

/// Local Beekeeper passwords are encrypted with a separate application key.
/// Read only connection metadata; never attempt to interpret ciphertext as a password.
pub(super) fn read_beekeeper_db(path: &std::path::Path) -> Result<ImportBatch, ImportError> {
    use rusqlite::{Connection, OpenFlags, types::ValueRef};
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| ImportError::BeekeeperOpen)?;
    db.busy_timeout(std::time::Duration::from_millis(250))
        .map_err(|_| ImportError::BeekeeperRead)?;
    let mut stmt = db
        .prepare("SELECT * FROM saved_connection WHERE workspaceId = -1 ORDER BY id")
        .map_err(|_| ImportError::BeekeeperUnsupported)?;
    let names: Vec<_> = stmt
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut rows = stmt
        .query([])
        .map_err(|_| ImportError::BeekeeperConnections)?;
    let mut values = Vec::new();
    while let Some(row) = rows.next().map_err(|_| ImportError::BeekeeperConnections)? {
        let mut obj = serde_json::Map::new();
        for (index, name) in names.iter().enumerate() {
            if !matches!(
                name.as_str(),
                "name"
                    | "connectionType"
                    | "host"
                    | "port"
                    | "username"
                    | "defaultDatabase"
                    | "ssl"
                    | "sslRejectUnauthorized"
                    | "sslCaFile"
                    | "sslCertFile"
                    | "sslKeyFile"
                    | "sshEnabled"
                    | "sshHost"
                    | "sshPort"
                    | "sshUsername"
                    | "sshMode"
                    | "sshKeyfile"
                    | "sshBastionHost"
            ) {
                continue;
            }
            let value = match row
                .get_ref(index)
                .map_err(|_| ImportError::BeekeeperSettings)?
            {
                ValueRef::Integer(n)
                    if matches!(
                        name.as_str(),
                        "ssl" | "sslRejectUnauthorized" | "sshEnabled"
                    ) =>
                {
                    json!(n != 0)
                }
                ValueRef::Integer(n) => json!(n),
                ValueRef::Text(s) => json!(String::from_utf8_lossy(s)),
                _ => Value::Null,
            };
            obj.insert(name.clone(), value);
        }
        values.push(Value::Object(obj));
        if values.len() > 10_000 {
            return Err(ImportError::BeekeeperTooMany);
        }
    }
    let bytes = serde_json::to_vec(&values).map_err(|_| ImportError::BeekeeperConnections)?;
    let mut batch = decode(ClientFormat::Beekeeper, &bytes)?;
    batch.warnings.push("Beekeeper app.db passwords are encrypted and are not imported. Use plaintext connection JSON to transfer credentials.".into());
    Ok(batch)
}
