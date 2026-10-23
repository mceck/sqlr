use serde_json::json;
use sqlr_core::ConnectionConfig;

use super::{ClientFormat, ExportDocument, portable};

fn val<'a>(c: &'a ConnectionConfig, k: &str) -> &'a str {
    c.values.get(k).map(String::as_str).unwrap_or_default()
}
fn default_port(d: &str) -> u16 {
    match d {
        "postgres" => 5432,
        "mysql" => 3306,
        "mssql" => 1433,
        "oracle" => 1521,
        "redis" => 6379,
        "mongo" => 27017,
        _ => 0,
    }
}
fn port(c: &ConnectionConfig) -> u16 {
    val(c, "port")
        .parse()
        .unwrap_or_else(|_| default_port(&c.driver))
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn jdbc_url(c: &ConnectionConfig) -> Result<String, String> {
    if c.driver == "sqlite" {
        return Ok(format!("jdbc:sqlite:{}", val(c, "path")));
    }
    let mut u =
        url::Url::parse(&connection_url(c, false)?).map_err(|_| "Invalid connection URL.")?;
    u.set_username("").map_err(|_| "Invalid JDBC URL.")?;
    if c.driver == "mssql" {
        let authority = &u[url::Position::BeforeHost..url::Position::AfterPort];
        let db = val(c, "database");
        if db.contains([';', '{', '}']) {
            return Err("SQL Server database name needs manual JDBC URL configuration.".into());
        }
        let encryption = if val(c, "encryption") == "disabled" {
            "false"
        } else {
            "true"
        };
        return Ok(format!(
            "jdbc:sqlserver://{authority};databaseName={db};encrypt={encryption};trustServerCertificate={}",
            val(c, "trust_server_certificate") == "true"
        ));
    }
    Ok(format!("jdbc:{u}"))
}

fn connection_url(c: &ConnectionConfig, tableplus: bool) -> Result<String, String> {
    let scheme =
        match c.driver.as_str() {
            "postgres" => "postgresql",
            "mysql" => "mysql",
            "sqlite" => "sqlite",
            "mssql" => "mssql",
            // Complex Oracle/JDBC, document and HTTP configurations need their own adapters.
            _ => return Err(
                "This driver cannot be represented by this client's portable connection format."
                    .into(),
            ),
        };
    let mut u = url::Url::parse(&format!("{scheme}://localhost"))
        .map_err(|_| "Cannot build connection URL.")?;
    if c.driver == "sqlite" {
        u.set_host(None).map_err(|_| "Cannot build SQLite URL.")?;
        u.set_path(val(c, "path"));
    } else {
        let host = val(c, "host");
        let host = if host.is_empty() {
            "localhost".to_string()
        } else if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        u.set_host(Some(&host))
            .map_err(|_| "Invalid connection host.")?;
        u.set_port(Some(port(c)))
            .map_err(|_| "Invalid connection port.")?;
        u.set_username(val(c, "user"))
            .map_err(|_| "Invalid connection user.")?;
        u.set_path(&format!("/{}", val(c, "database")));
    }
    {
        let mut q = u.query_pairs_mut();
        for (from, to) in [
            ("ssl_mode", "sslmode"),
            ("ssl_root_cert", "sslrootcert"),
            ("ssl_cert", "sslcert"),
            ("ssl_key", "sslkey"),
        ] {
            let v = val(c, from);
            if !v.is_empty() {
                if c.driver == "mysql" && from == "ssl_mode" {
                    q.append_pair("sslMode", &v.to_ascii_uppercase());
                } else {
                    q.append_pair(to, v);
                }
            }
        }
        if tableplus {
            q.append_pair("name", &c.name);
            if let Some(profile) = &c.profile {
                q.append_pair("env", profile);
            }
            if let Some([r, g, b]) = c.color {
                q.append_pair("statusColor", &format!("{r:02X}{g:02X}{b:02X}"));
            }
        }
    }
    if c.driver == "sqlite" {
        Ok(u.to_string().replacen("sqlite:", "sqlite://", 1))
    } else {
        Ok(u.to_string())
    }
}

pub fn encode(
    format: ClientFormat,
    connections: &[ConnectionConfig],
) -> Result<ExportDocument, String> {
    if connections.is_empty() {
        return Err("Select at least one connection.".into());
    }
    let mut warnings = Vec::new();
    let mut configs = Vec::new();
    for c in connections {
        if format == ClientFormat::PgAdmin && c.driver != "postgres" {
            warnings.push(format!(
                "{}: pgAdmin supports only PostgreSQL; excluded.",
                c.name
            ));
            continue;
        }
        if matches!(format, ClientFormat::Datagrip | ClientFormat::TablePlus)
            && connection_url(c, false).is_err()
        {
            warnings.push(format!(
                "{}: this driver has no supported portable URL; excluded.",
                c.name
            ));
            continue;
        }
        if format != ClientFormat::Sqlr
            && c.values.get("ssh_enabled").is_some_and(|v| v == "true")
            && matches!(format, ClientFormat::TablePlus | ClientFormat::Datagrip)
        {
            warnings.push(format!(
                "{}: SSH is not portable to this format; excluded.",
                c.name
            ));
            continue;
        }
        configs.push(portable(c.clone()));
    }
    if configs.is_empty() {
        return Err("None of the selected connections can be exported to this format. Choose sqlr to back up all drivers and portable settings.".into());
    }
    if format != ClientFormat::Sqlr {
        warnings.push("Client-specific preferences and custom sqlr profile definitions are not exported. Review the target client's connection settings.".into());
    }
    let value = match format {
        ClientFormat::Sqlr => Some(json!({"sqlr_version":1,"connections":configs})),
        ClientFormat::Dbeaver => {
            let mut map = serde_json::Map::new();
            for (i, c) in configs.iter().enumerate() {
                let (provider, driver) = match c.driver.as_str() {
                    "postgres" => ("postgresql", "postgres-jdbc"),
                    "mysql" => ("mysql", "mysql8"),
                    "sqlite" => ("sqlite", "sqlite_jdbc"),
                    "mssql" => ("sqlserver", "microsoft"),
                    "oracle" => ("oracle", "oracle_thin"),
                    _ => {
                        warnings.push(format!("{}: unsupported DBeaver driver; excluded.", c.name));
                        continue;
                    }
                };
                let database = if c.driver == "sqlite" {
                    val(c, "path")
                } else if c.driver == "oracle" {
                    val(c, "service")
                } else {
                    val(c, "database")
                };
                let environment = match c.profile.as_deref() {
                    Some("production") => "prod",
                    Some("test") => "test",
                    _ => "dev",
                };
                let mut properties = serde_json::Map::new();
                for (from, to) in [
                    ("ssl_mode", "sslmode"),
                    ("ssl_root_cert", "sslrootcert"),
                    ("ssl_cert", "sslcert"),
                    ("ssl_key", "sslkey"),
                ] {
                    if !val(c, from).is_empty() && c.driver == "postgres" {
                        properties.insert(to.into(), json!(val(c, from)));
                    }
                }
                if c.driver == "mysql" && !val(c, "ssl_mode").is_empty() {
                    properties.insert(
                        "sslMode".into(),
                        json!(val(c, "ssl_mode").to_ascii_uppercase()),
                    );
                }
                if c.driver == "mssql" {
                    properties.insert("encrypt".into(), json!(val(c, "encryption") != "disabled"));
                    properties.insert(
                        "trustServerCertificate".into(),
                        json!(val(c, "trust_server_certificate") == "true"),
                    );
                }
                let mut cfg = json!({"host":val(c,"host"),"port":port(c).to_string(),"database":database,
                    "user":val(c,"user"),"auth-model":"native","auth-properties":{"userName":val(c,"user")},"configurationType":"MANUAL",
                    "type":environment,"properties":properties,"handlers":{}});
                if val(c, "ssh_enabled") == "true" {
                    cfg["handlers"]["ssh_tunnel"] = json!({"type":"TUNNEL","enabled":true,"properties":{
                        "host":val(c,"ssh_host"),"port":val(c,"ssh_port"),"user":val(c,"ssh_user"),"keyPath":val(c,"ssh_key_path"),
                        "authType":if val(c,"ssh_auth") == "key" { "PUBLIC_KEY" } else { "PASSWORD" }
                    }});
                }
                if matches!(c.driver.as_str(), "postgres" | "mysql")
                    && !val(c, "ssl_mode").is_empty()
                {
                    let id = if c.driver == "postgres" {
                        "postgre_ssl"
                    } else {
                        "mysql_ssl"
                    };
                    let mode = match val(c, "ssl_mode") {
                        "disabled" => "disable",
                        "preferred" => "prefer",
                        "required" => "require",
                        "verify_ca" => "verify-ca",
                        "verify_identity" => "verify-full",
                        s => s,
                    };
                    let ca = if c.driver == "postgres" {
                        val(c, "ssl_root_cert")
                    } else {
                        val(c, "ssl_ca")
                    };
                    cfg["handlers"][id] = json!({"type":"CONFIG","enabled":true,"properties":{
                        "ssl.method":"CERTIFICATES","ssl.sslMode":mode,"ssl.ca.cert":ca,
                        "ssl.client.cert":val(c,"ssl_cert"),"ssl.client.key":val(c,"ssl_key")
                    }});
                }
                map.insert(format!("sqlr-{i}-{}",c.id),json!({"provider":provider,"driver":driver,"name":c.name,"folder":c.group,"save-password":false,"configuration":cfg}));
            }
            if map.is_empty() {
                return Err("No selected connection has a supported DBeaver driver.".into());
            }
            Some(json!({"folders":{},"connections":map}))
        }
        ClientFormat::PgAdmin => {
            let mut map = serde_json::Map::new();
            for (i, c) in configs.iter().enumerate() {
                let mode = if val(c, "ssl_mode").is_empty() {
                    "prefer"
                } else {
                    val(c, "ssl_mode")
                };
                let mut row = json!({"Name":c.name,"Group":c.group.as_deref().unwrap_or("sqlr"),
                    "Host":val(c,"host"),"Port":port(c),"Username":val(c,"user"),"MaintenanceDB":val(c,"database"),
                    "SSLMode":mode,"ConnectionParameters":{"sslmode":mode,"sslrootcert":val(c,"ssl_root_cert"),"sslcert":val(c,"ssl_cert"),"sslkey":val(c,"ssl_key")}});
                if val(c, "ssh_enabled") == "true" {
                    row["UseSSHTunnel"] = json!(1);
                    for (from, to) in [
                        ("ssh_host", "TunnelHost"),
                        ("ssh_port", "TunnelPort"),
                        ("ssh_user", "TunnelUsername"),
                        ("ssh_key_path", "TunnelIdentityFile"),
                    ] {
                        row[to] = json!(val(c, from));
                    }
                    row["TunnelAuthentication"] =
                        json!(if val(c, "ssh_auth") == "key" { 1 } else { 0 });
                }
                map.insert((i + 1).to_string(), row);
            }
            Some(json!({"Servers":map}))
        }
        _ => None,
    };
    let contents = if let Some(value) = value {
        serde_json::to_string_pretty(&value).map_err(|_| "Could not serialize connections.")?
    } else if format == ClientFormat::Datagrip {
        let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<data-sources>\n");
        for c in &configs {
            let u = jdbc_url(c)?;
            let d = match c.driver.as_str() {
                "postgres" => "postgresql",
                "mysql" => "mysql.8",
                "mssql" => "sqlserver.jb",
                _ => "sqlite.xerial",
            };
            s.push_str(&format!("  <data-source source=\"LOCAL\" name=\"{}\" uuid=\"{}\" group=\"{}\">\n    <driver-ref>{d}</driver-ref>\n    <jdbc-url>{}</jdbc-url>\n    <user-name>{}</user-name>\n  </data-source>\n",xml(&c.name),xml(&c.id),xml(c.group.as_deref().unwrap_or_default()),xml(&u),xml(val(c,"user"))));
        }
        s.push_str("</data-sources>\n");
        s
    } else {
        let mut s = String::new();
        for c in &configs {
            s.push_str(&connection_url(c, true)?);
            s.push('\n');
        }
        s
    };
    Ok(ExportDocument { contents, warnings })
}
