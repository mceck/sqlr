use serde_json::Value;
use sqlr_core::{ConnectionConfig, ConnectionProfile, parse_connection_url};

use super::{ClientFormat, ImportBatch, ImportError, blank, driver, portable};

fn text(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}
fn put(c: &mut ConnectionConfig, key: &str, value: impl Into<String>) {
    let value = value.into();
    if !value.is_empty() {
        c.values.insert(key.into(), value);
    }
}
fn fields(c: &mut ConnectionConfig, v: &Value, pairs: &[(&str, &str)]) {
    for (from, to) in pairs {
        put(c, to, text(v, from));
    }
}
fn ssh_auth(c: &mut ConnectionConfig) {
    let auth = if c.values.contains_key("ssh_key_path") {
        "key"
    } else {
        "password"
    };
    put(c, "ssh_auth", auth);
}

fn tls_mode(c: &mut ConnectionConfig, mode: &str) {
    let mode = mode.to_ascii_lowercase().replace('_', "-");
    let mode = if c.driver == "mysql" {
        match mode.as_str() {
            "disable" | "disabled" => "disabled",
            "prefer" | "preferred" => "preferred",
            "require" | "required" => "required",
            "verify-ca" => "verify_ca",
            "verify-full" | "verify-identity" => "verify_identity",
            _ => &mode,
        }
    } else {
        &mode
    };
    put(c, "ssl_mode", mode);
}
fn group(c: &mut ConnectionConfig, value: String) {
    c.group = Some(value).filter(|s| !s.is_empty());
}
fn profile(c: &mut ConnectionConfig, env: &str) {
    c.profile = match env.to_ascii_lowercase().as_str() {
        "prod" | "production" => Some(ConnectionProfile::PRODUCTION.into()),
        "test" | "testing" | "staging" => Some(ConnectionProfile::TEST.into()),
        _ => None,
    };
}
fn color(c: &mut ConnectionConfig, s: &str) {
    let s = s.trim_start_matches('#');
    if s.len() == 6 && s.is_ascii() {
        c.color = u32::from_str_radix(s, 16)
            .ok()
            .map(|n| [(n >> 16) as u8, (n >> 8) as u8, n as u8]);
    }
}

pub fn detect(bytes: &[u8]) -> Result<ClientFormat, ImportError> {
    if bytes.starts_with(b"bplist00") {
        return Ok(ClientFormat::TablePlus);
    }
    let s = std::str::from_utf8(bytes).map_err(|_| ImportError::EncryptedTablePlus)?;
    let s = s.trim_start_matches('\u{feff}').trim();
    if s.starts_with('<') {
        return if s.contains("<plist") {
            Ok(ClientFormat::TablePlus)
        } else if s.contains("<data-source") {
            Ok(ClientFormat::Datagrip)
        } else {
            Err(ImportError::UnknownXml)
        };
    }
    if s.starts_with('{') || s.starts_with('[') {
        let v: Value = serde_json::from_str(s).map_err(|_| ImportError::InvalidJson)?;
        if v.get("encrypted").is_some() || v.get("ciphertext").is_some() {
            return Err(ImportError::EncryptedBackup);
        }
        if v["connections"].is_array()
            && (v.get("version").is_some() || v.get("groups").is_some())
            && v.get("sqlr_version").is_none()
        {
            return Ok(ClientFormat::Tabularis);
        }
        let first = v.as_array().and_then(|a| a.first()).unwrap_or(&v);
        if first.get("connectionType").is_some() || first.get("connection_type").is_some() {
            return Ok(ClientFormat::Beekeeper);
        }
        if v.get("Servers").is_some() {
            return Ok(ClientFormat::PgAdmin);
        }
        if v.get("sqlr_version").is_some() {
            return Ok(ClientFormat::Sqlr);
        }
        if v["connections"].is_object() {
            return Ok(ClientFormat::Dbeaver);
        }
        if v.is_array() {
            return Ok(ClientFormat::TablePlus);
        }
    }
    if s.lines().any(|l| l.trim().contains("://")) {
        return Ok(ClientFormat::TablePlus);
    }
    Err(ImportError::Unrecognized)
}

pub fn decode(format: ClientFormat, bytes: &[u8]) -> Result<ImportBatch, ImportError> {
    let mut batch = match format {
        ClientFormat::Beekeeper | ClientFormat::Tabularis => {
            super::native_clients::decode(format, bytes)
        }
        ClientFormat::Datagrip => datagrip(bytes),
        ClientFormat::TablePlus => tableplus(bytes),
        _ => {
            let v: Value = serde_json::from_slice(bytes).map_err(|_| ImportError::InvalidJson)?;
            match format {
                ClientFormat::Sqlr => {
                    if v["sqlr_version"] != 1 && v["sqlr_version"] != 2 {
                        return Err(ImportError::UnsupportedSqlrVersion);
                    }
                    let connections: Vec<ConnectionConfig> =
                        serde_json::from_value(v["connections"].clone())
                            .map_err(|_| ImportError::InvalidSqlrConnections)?;
                    let mut credentials = std::collections::BTreeMap::new();
                    let mut configs = Vec::new();
                    for (index, c) in connections.into_iter().enumerate() {
                        let mut cfg = portable(c);
                        if let Some(value) = v["credentials"].get(&cfg.id) {
                            let fields: std::collections::BTreeMap<String, String> =
                                serde_json::from_value(value.clone())
                                    .map_err(|_| ImportError::InvalidBackupCredentials)?;
                            credentials.insert(index, sqlr_core::Secrets(fields));
                        }
                        cfg.id.clear();
                        configs.push(cfg);
                    }
                    Ok(ImportBatch {
                        connections: configs,
                        credentials,
                        warnings: vec![],
                        origins: vec![],
                    })
                }
                ClientFormat::Dbeaver => dbeaver(&v),
                ClientFormat::PgAdmin => pgadmin(&v),
                _ => unreachable!(),
            }
        }
    }?;
    // Never reuse foreign identifiers or carry explicit agent consent across a migration.
    for c in &mut batch.connections {
        c.id.clear();
    }
    Ok(batch)
}

/// Convert endpoint URLs without ever persisting the URL or its embedded credentials.
/// Drop passwords before handing the endpoint to the core's strict parser.
fn url_into(c: &mut ConnectionConfig, raw: &str) -> Result<(), ImportError> {
    let raw = raw.strip_prefix("jdbc:").unwrap_or(raw);
    // JDBC SQLite paths are not URL-encoded, unlike sqlite: URLs.
    if c.driver == "sqlite" && !raw.starts_with("sqlite://") {
        put(c, "path", raw.strip_prefix("sqlite:").unwrap_or(raw));
        return Ok(());
    }
    if let Some(rest) = raw.strip_prefix("sqlserver://") {
        let mut parts = rest.split(';');
        let base = format!("mssql://{}", parts.next().unwrap_or_default());
        url_into(c, &base)?;
        for item in parts {
            let Some((k, v)) = item.split_once('=') else {
                continue;
            };
            match k.to_ascii_lowercase().as_str() {
                "databasename" => put(c, "database", v),
                "user" | "username" => put(c, "user", v),
                "encrypt" => put(
                    c,
                    "encryption",
                    if v.eq_ignore_ascii_case("false") {
                        "disabled"
                    } else {
                        "required"
                    },
                ),
                "trustservercertificate" => put(c, "trust_server_certificate", v),
                "password" => {}
                _ => {}
            }
        }
        return Ok(());
    }
    if let Some(rest) = raw.strip_prefix("oracle:thin:@//") {
        let (authority, service) = rest
            .split_once('/')
            .ok_or(ImportError::UnsupportedOracleUrl)?;
        url_into(c, &format!("oracle://{authority}"))?;
        put(c, "service", service);
        return Ok(());
    }
    let mut safe = url::Url::parse(raw).map_err(|_| ImportError::UnsupportedUrl)?;
    if safe.password().is_some() {
        safe.set_password(None)
            .map_err(|_| ImportError::UnsupportedUrl)?;
    }
    if safe.scheme() == "mongodb+srv" {
        return Err(ImportError::MongoSrv);
    }
    let pairs: Vec<(String, String)> = safe
        .query_pairs()
        .filter(|(key, _)| {
            let key = key.to_ascii_lowercase();
            !["password", "passwd", "pwd", "secret", "token", "apikey"]
                .iter()
                .any(|s| key.contains(s))
                && key != "pass"
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    safe.set_query(None);
    if !pairs.is_empty() {
        safe.query_pairs_mut().extend_pairs(&pairs);
    }
    let raw = safe.as_str();
    let parsed = parse_connection_url(raw).map_err(|_| ImportError::CredentialBearingUrl)?;
    if parsed.scheme == "rediss" {
        put(c, "tls", "required");
    }
    if c.driver == "elasticsearch" {
        put(c, "scheme", &parsed.scheme);
    }
    if let Some(v) = parsed.host {
        put(c, "host", v);
    }
    if let Some(v) = parsed.port {
        put(c, "port", v.to_string());
    }
    if let Some(v) = parsed.database {
        put(c, "database", v);
    }
    if let Some(v) = parsed.path {
        put(c, "path", v);
    }
    if let Some(v) = parsed.user {
        put(c, "user", v);
    }
    let params: Vec<(String, String)> = raw
        .split_once('?')
        .map(|(_, q)| {
            url::form_urlencoded::parse(q.split('#').next().unwrap_or_default().as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or(parsed.params);
    for (k, v) in params {
        match k.as_str() {
            "sslmode" | "ssl_mode" | "sslMode" => tls_mode(c, &v),
            "sslrootcert" => put(c, "ssl_root_cert", v),
            "sslcert" => put(c, "ssl_cert", v),
            "sslkey" => put(c, "ssl_key", v),
            "name" => c.name = v,
            "env" => profile(c, &v),
            "statusColor" => color(c, &v),
            "ssh_host" | "ssh_port" | "ssh_user" | "ssh_key_path" | "ssh_auth" | "ssh_enabled" => {
                put(c, &k, v)
            }
            _ => {}
        }
    }
    Ok(())
}

fn dbeaver(v: &Value) -> Result<ImportBatch, ImportError> {
    let map = v["connections"]
        .as_object()
        .ok_or(ImportError::NoDbeaverConnections)?;
    let mut b = ImportBatch::default();
    for row in map.values() {
        let name = text(row, "name");
        let Some(d) = driver(&text(row, "driver")).or_else(|| driver(&text(row, "provider")))
        else {
            b.warnings
                .push(format!("{name}: unsupported database driver; skipped."));
            continue;
        };
        let mut c = blank(&name, d);
        let cfg = &row["configuration"];
        if !text(cfg, "config-profile").is_empty() {
            b.warnings.push(format!(
                "{name}: external network profile requires manual setup; skipped."
            ));
            continue;
        }
        let auth_model = text(cfg, "auth-model");
        if !auth_model.is_empty()
            && !matches!(
                auth_model.as_str(),
                "native"
                    | "oracle_native"
                    | "sqlserver_database"
                    | "mongodb_default"
                    | "redis_password"
            )
        {
            b.warnings.push(format!(
                "{name}: authentication method requires manual setup; skipped."
            ));
            continue;
        }
        fields(
            &mut c,
            cfg,
            &[
                ("host", "host"),
                ("port", "port"),
                ("database", "database"),
                ("user", "user"),
            ],
        );
        put(&mut c, "user", text(&cfg["auth-properties"], "user"));
        put(&mut c, "user", text(&cfg["auth-properties"], "userName"));
        let u = text(cfg, "url");
        if !u.is_empty()
            && let Err(e) = url_into(&mut c, &u)
        {
            if cfg["configurationType"] == "URL" || c.values.is_empty() {
                b.warnings.push(format!("{name}: {e} Skipped."));
                continue;
            }
            b.warnings.push(format!(
                "{name}: URL could not be migrated; used explicit host/database fields."
            ));
        }
        if d == "sqlite"
            && let Some(path) = c.values.remove("database")
        {
            put(&mut c, "path", path);
        }
        if d == "oracle"
            && !c.values.contains_key("service")
            && let Some(service) = c.values.remove("database")
        {
            put(&mut c, "service", service);
        }
        fields(
            &mut c,
            &cfg["properties"],
            &[
                ("sslmode", "ssl_mode"),
                ("sslrootcert", "ssl_root_cert"),
                ("sslcert", "ssl_cert"),
                ("sslkey", "ssl_key"),
            ],
        );
        group(&mut c, text(row, "folder"));
        if let Some(mode) = c.values.get("ssl_mode").cloned() {
            tls_mode(&mut c, &mode);
        }
        let mysql_mode = text(&cfg["properties"], "sslMode");
        if !mysql_mode.is_empty() {
            tls_mode(&mut c, &mysql_mode);
        }
        profile(&mut c, &text(cfg, "type"));
        color(&mut c, &text(row, "color"));
        if row["read-only"] == true {
            b.warnings.push(format!(
                "{name}: read-only policy needs a sqlr profile; skipped to preserve that policy."
            ));
            continue;
        }
        if let Some(handlers) = cfg["handlers"].as_object() {
            let mut unsupported = false;
            for (id, h) in handlers {
                if h["enabled"] != true {
                    continue;
                }
                if id == "ssh_tunnel" {
                    if text(&h["properties"], "jumpServer.count")
                        .parse::<usize>()
                        .unwrap_or(0)
                        > 0
                    {
                        unsupported = true;
                        continue;
                    }
                    put(&mut c, "ssh_enabled", "true");
                    fields(
                        &mut c,
                        &h["properties"],
                        &[
                            ("host", "ssh_host"),
                            ("port", "ssh_port"),
                            ("user", "ssh_user"),
                            ("keyPath", "ssh_key_path"),
                        ],
                    );
                    let auth = if c.values.contains_key("ssh_key_path") {
                        "key"
                    } else {
                        "password"
                    };
                    put(&mut c, "ssh_auth", auth);
                } else if (id == "postgre_ssl" && d == "postgres")
                    || (id == "mysql_ssl" && d == "mysql")
                {
                    let props = &h["properties"];
                    let mode = text(props, "ssl.sslMode");
                    let method = text(props, "ssl.method");
                    if mode.is_empty()
                        || method == "KEYSTORE"
                        || [
                            "ssl.client.key.value",
                            "ssl.client.cert.value",
                            "ssl.ca.cert.value",
                        ]
                        .iter()
                        .any(|key| !text(props, key).is_empty())
                    {
                        unsupported = true;
                        continue;
                    }
                    tls_mode(&mut c, &mode);
                    let ca_key = if d == "mysql" {
                        "ssl_ca"
                    } else {
                        "ssl_root_cert"
                    };
                    fields(
                        &mut c,
                        props,
                        &[
                            ("ssl.ca.cert", ca_key),
                            ("ssl.client.cert", "ssl_cert"),
                            ("ssl.client.key", "ssl_key"),
                        ],
                    );
                } else {
                    unsupported = true;
                }
            }
            if unsupported {
                b.warnings.push(format!(
                    "{name}: unsupported network handler; skipped to preserve connection security."
                ));
                continue;
            }
        }
        b.connections.push(c);
    }
    Ok(b)
}

fn xml_text(node: roxmltree::Node<'_, '_>, tag: &str) -> String {
    node.descendants()
        .find(|n| n.has_tag_name(tag))
        .and_then(|n| n.text())
        .unwrap_or_default()
        .to_string()
}

fn datagrip(bytes: &[u8]) -> Result<ImportBatch, ImportError> {
    let s = std::str::from_utf8(bytes).map_err(|_| ImportError::InvalidXmlEncoding)?;
    let doc = roxmltree::Document::parse(s).map_err(|_| ImportError::InvalidDatagripXml)?;
    let mut b = ImportBatch::default();
    for n in doc.descendants().filter(|n| n.has_tag_name("data-source")) {
        let name = n.attribute("name").unwrap_or("Imported connection");
        let url = xml_text(n, "jdbc-url");
        let dr = xml_text(n, "driver-ref");
        let hint = url
            .strip_prefix("jdbc:")
            .unwrap_or(&url)
            .split(':')
            .next()
            .unwrap_or_default();
        let Some(d) = driver(&dr).or_else(|| driver(hint)) else {
            b.warnings
                .push(format!("{name}: unsupported database driver; skipped."));
            continue;
        };
        let mut c = blank(name, d);
        if let Err(e) = url_into(&mut c, &url) {
            b.warnings.push(format!("{name}: {e} Skipped."));
            continue;
        }
        put(&mut c, "user", xml_text(n, "user-name"));
        group(&mut c, n.attribute("group").unwrap_or_default().into());
        // Keep a temporary foreign id for merging the optional sibling local file.
        c.id = n.attribute("uuid").unwrap_or_default().into();
        if n.descendants().any(|n| {
            (n.has_tag_name("ssh-settings") || n.has_tag_name("ssl-config"))
                && n.attribute("enabled") == Some("true")
        }) {
            b.warnings.push(format!(
                "{name}: embedded SSH/TLS settings require manual review; skipped."
            ));
            continue;
        }
        b.connections.push(c);
    }
    Ok(b)
}

pub(super) fn merge_datagrip_local(
    batch: &mut ImportBatch,
    bytes: &[u8],
) -> Result<(), ImportError> {
    let s = std::str::from_utf8(bytes).map_err(|_| ImportError::InvalidLocalDatagripEncoding)?;
    let doc = roxmltree::Document::parse(s).map_err(|_| ImportError::InvalidLocalDatagripXml)?;
    let mut excluded = Vec::new();
    for c in &mut batch.connections {
        let Some(n) = doc
            .descendants()
            .find(|n| n.has_tag_name("data-source") && n.attribute("uuid") == Some(c.id.as_str()))
        else {
            continue;
        };
        put(c, "user", xml_text(n, "user-name"));
        if let Some(ssh) = n
            .descendants()
            .find(|n| n.has_tag_name("ssh-settings") && n.attribute("enabled") == Some("true"))
        {
            put(c, "ssh_enabled", "true");
            for (a, k) in [
                ("host", "ssh_host"),
                ("port", "ssh_port"),
                ("user", "ssh_user"),
                ("key-path", "ssh_key_path"),
            ] {
                put(c, k, ssh.attribute(a).unwrap_or_default());
            }
            put(
                c,
                "ssh_auth",
                if c.values.contains_key("ssh_key_path") {
                    "key"
                } else {
                    "password"
                },
            );
        }
        if n.descendants()
            .any(|n| n.has_tag_name("ssl-config") && n.attribute("enabled") == Some("true"))
        {
            batch.warnings.push(format!(
                "{}: local TLS configuration requires manual setup; skipped to preserve verification settings.",
                c.name
            ));
            excluded.push(c.id.clone());
        }
    }
    batch.connections.retain(|c| !excluded.contains(&c.id));
    Ok(())
}

pub(super) fn datagrip_file(
    bytes: &[u8],
    local: Option<&[u8]>,
) -> Result<ImportBatch, ImportError> {
    let mut b = datagrip(bytes)?;
    if let Some(bytes) = local {
        merge_datagrip_local(&mut b, bytes)?;
    }
    for c in &mut b.connections {
        c.id.clear();
    }
    Ok(b)
}

fn tableplus(bytes: &[u8]) -> Result<ImportBatch, ImportError> {
    let s = std::str::from_utf8(bytes).unwrap_or_default().trim();
    if bytes.starts_with(b"bplist00") || s.contains("<plist") {
        let v: Value = plist::from_bytes(bytes).map_err(|_| ImportError::InvalidTablePlusPlist)?;
        return tableplus_rows(&v);
    }
    if s.starts_with('[') {
        let v: Value =
            serde_json::from_slice(bytes).map_err(|_| ImportError::InvalidTablePlusJson)?;
        return tableplus_rows(&v);
    }
    let mut b = ImportBatch::default();
    if !s.lines().any(|l| l.contains("://")) {
        return Err(ImportError::NoConnectionUrls);
    }
    for (i, line) in s.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let scheme = line.split(':').next().unwrap_or_default();
        let Some(d) = driver(scheme) else {
            b.warnings.push(format!(
                "URL {}: unsupported database driver; skipped.",
                i + 1
            ));
            continue;
        };
        let mut c = blank("Imported connection", d);
        if url_into(&mut c, line).is_err() {
            b.warnings.push(format!(
                "URL {}: invalid or credential-bearing URL; skipped.",
                i + 1
            ));
            continue;
        }
        b.connections.push(c);
    }
    if b.connections.is_empty() && b.warnings.is_empty() {
        return Err(ImportError::NoConnectionUrls);
    }
    Ok(b)
}

fn tableplus_rows(v: &Value) -> Result<ImportBatch, ImportError> {
    let rows = v.as_array().ok_or(ImportError::ExpectedTablePlusArray)?;
    let mut b = ImportBatch::default();
    for row in rows {
        let name = text(row, "ConnectionName");
        let Some(d) = driver(&text(row, "Driver")) else {
            b.warnings
                .push(format!("{name}: unsupported database driver; skipped."));
            continue;
        };
        let mut c = blank(&name, d);
        fields(
            &mut c,
            row,
            &[
                ("DatabaseHost", "host"),
                ("DatabasePort", "port"),
                ("DatabaseUser", "user"),
                ("DatabaseName", "database"),
                ("DatabasePath", "path"),
            ],
        );
        profile(&mut c, &text(row, "Enviroment"));
        if c.profile.is_none() {
            profile(&mut c, &text(row, "Environment"));
        }
        color(&mut c, &text(row, "StatusColor"));
        if row["OverSSH"] == true || row["OverSSH"] == 1 {
            put(&mut c, "ssh_enabled", "true");
            fields(
                &mut c,
                row,
                &[
                    ("SSHHost", "ssh_host"),
                    ("SSHPort", "ssh_port"),
                    ("SSHUser", "ssh_user"),
                    ("SSHKeyPath", "ssh_key_path"),
                ],
            );
            ssh_auth(&mut c);
        }
        // Numeric TLS modes vary between TablePlus drivers; do not guess their meaning.
        if row.get("TLSMode").is_some_and(|m| m != 0 && m != "0") {
            b.warnings.push(format!(
                "{name}: TablePlus TLS mode needs manual configuration; skipped."
            ));
            continue;
        }
        b.connections.push(c);
    }
    Ok(b)
}

fn pgadmin(v: &Value) -> Result<ImportBatch, ImportError> {
    let rows = v["Servers"]
        .as_object()
        .ok_or(ImportError::NoPgAdminServers)?;
    let mut b = ImportBatch::default();
    for row in rows.values() {
        let mut c = blank(&text(row, "Name"), "postgres");
        fields(
            &mut c,
            row,
            &[
                ("Host", "host"),
                ("Port", "port"),
                ("Username", "user"),
                ("MaintenanceDB", "database"),
                ("SSLMode", "ssl_mode"),
                ("SSLCert", "ssl_cert"),
                ("SSLKey", "ssl_key"),
                ("SSLRootCert", "ssl_root_cert"),
            ],
        );
        fields(
            &mut c,
            &row["ConnectionParameters"],
            &[
                ("sslmode", "ssl_mode"),
                ("sslrootcert", "ssl_root_cert"),
                ("sslcert", "ssl_cert"),
                ("sslkey", "ssl_key"),
            ],
        );
        if !c.values.contains_key("host") {
            put(&mut c, "host", text(row, "HostAddr"));
        }
        if !c.values.contains_key("host") {
            b.warnings.push(format!(
                "{}: service-only definitions are not supported; skipped.",
                c.name
            ));
            continue;
        }
        group(&mut c, text(row, "Group"));
        color(&mut c, &text(row, "BGColor"));
        if row["UseSSHTunnel"] == true || row["UseSSHTunnel"] == 1 {
            put(&mut c, "ssh_enabled", "true");
            fields(
                &mut c,
                row,
                &[
                    ("TunnelHost", "ssh_host"),
                    ("TunnelPort", "ssh_port"),
                    ("TunnelUsername", "ssh_user"),
                    ("TunnelIdentityFile", "ssh_key_path"),
                ],
            );
            ssh_auth(&mut c);
        }
        b.connections.push(c);
    }
    Ok(b)
}
