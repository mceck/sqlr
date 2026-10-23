//! Connection URLs (`postgresql://user@host:5432/db?sslmode=require`, `sqlite:/path/to.db`).
//!
//! A URL fills the connection form; it is never stored. Passwords are refused so they only
//! ever reach the keychain.

use std::collections::BTreeMap;

use crate::manifest::{DriverManifest, FieldType};

const PASSWORD_REFUSED: &str = "Remove the password from the URL; enter it in the Password field";

/// The parts of a connection URL. `path` is set for file databases (`sqlite:`), the
/// network parts otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionUrl {
    /// Lowercase.
    pub scheme: String,
    pub user: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub path: Option<String>,
    /// Query parameters in order, percent-decoded.
    pub params: Vec<(String, String)>,
}

/// Schemes whose URL names a file instead of a server.
const FILE_SCHEMES: &[&str] = &["sqlite", "sqlite3", "file"];

fn percent_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| "Invalid percent-encoding in the URL".to_string())?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "Invalid UTF-8 in the URL".to_string())
}

fn is_password_key(key: &str) -> bool {
    let key = normalized(key);
    key == "pass"
        || key == "apikey"
        || ["password", "passwd", "pwd", "secret", "token"]
            .iter()
            .any(|word| key.contains(word))
}

/// Whether an authority without `@` may hold a half-typed `user:password`: something after
/// the last `:` that is not a port number.
fn may_hide_password(authority: &str) -> bool {
    if authority.contains('@') {
        return false;
    }
    let host_port = match authority.strip_prefix('[') {
        Some(v6) => v6.split_once(']').map_or("", |(_, tail)| tail),
        None => authority,
    };
    host_port
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && !port.bytes().all(|b| b.is_ascii_digit()))
}

fn parse_params(query: &str) -> Result<Vec<(String, String)>, String> {
    let mut params = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(k)?;
        if is_password_key(&key) {
            return Err(PASSWORD_REFUSED.into());
        }
        params.push((key, percent_decode(v)?));
    }
    Ok(params)
}

/// Parses `scheme://[user@]host[:port][/database][?k=v&…]` or `sqlite:[//]/path[?k=v]`.
/// A URL holding a password is refused.
pub fn parse_connection_url(url: &str) -> Result<ConnectionUrl, String> {
    let url = url.trim();
    let (scheme, rest) = url
        .split_once(':')
        .filter(|(s, _)| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        })
        .ok_or_else(|| format!("Not a connection URL: \"{}\"", redact_url(url)))?;
    let scheme = scheme.to_ascii_lowercase();
    // The fragment ends the query too.
    let rest = rest.split('#').next().unwrap_or_default();
    let (rest, query) = match rest.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (rest, None),
    };
    let params = parse_params(query.unwrap_or_default())?;

    if FILE_SCHEMES.contains(&scheme.as_str()) {
        // `sqlite:/abs`, `sqlite:///abs` and `sqlite://localhost/abs` all name `/abs`. Any
        // other authority (`sqlite://foo.db`, `sqlite://host/abs`) is refused: it would name
        // a file on another host, or a relative path that depends on the working directory.
        let path = match rest.strip_prefix("//") {
            Some(after) => {
                let slash = after.find('/').unwrap_or(after.len());
                let authority = &after[..slash];
                if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
                    return Err(format!(
                        "A {scheme} URL names a local file by its full path: \
                         {scheme}:///path/to/file.db"
                    ));
                }
                &after[slash..]
            }
            None => rest,
        };
        let path = percent_decode(path)?;
        return Ok(ConnectionUrl {
            scheme,
            user: None,
            host: None,
            port: None,
            database: None,
            path: (!path.is_empty()).then_some(path),
            params,
        });
    }

    let after = rest
        .strip_prefix("//")
        .ok_or_else(|| format!("Expected \"{scheme}://\" in the URL"))?;
    let (authority, database) = match after.find('/') {
        Some(slash) => (&after[..slash], Some(&after[slash + 1..])),
        None => (after, None),
    };
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(at) => (Some(&authority[..at]), &authority[at + 1..]),
        None => (None, authority),
    };
    let user = match userinfo {
        Some(info) => {
            if let Some((user, password)) = info.split_once(':') {
                if !password.is_empty() {
                    return Err(PASSWORD_REFUSED.into());
                }
                Some(percent_decode(user)?)
            } else {
                Some(percent_decode(info)?)
            }
        }
        None => None,
    };
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let (host, tail) = v6
            .split_once(']')
            .ok_or_else(|| "Unclosed \"[\" in the host".to_string())?;
        let port = match tail {
            "" => None,
            t => Some(
                t.strip_prefix(':')
                    .ok_or("Expected \":port\" after the host")?,
            ),
        };
        (host.to_string(), port)
    } else if hostport.matches(':').count() > 1 {
        // `::1` is not host `:` and port `1`. The text is not echoed: it may hold a password.
        return Err("Put an IPv6 address in brackets, as in [::1]:5432".into());
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (percent_decode(h)?, Some(p)),
            None => (percent_decode(hostport)?, None),
        }
    };
    // The port text is never echoed: before the `@` is typed it may be a password.
    let port = match port {
        None | Some("") => None,
        Some(p) => Some(
            p.parse::<u16>()
                .ok()
                .filter(|&p| p > 0)
                .ok_or("Invalid port: it must be a number from 1 to 65535")?,
        ),
    };
    let database = match database {
        Some(d) if !d.is_empty() => Some(percent_decode(d)?),
        _ => None,
    };
    Ok(ConnectionUrl {
        scheme,
        user: user.filter(|u| !u.is_empty()),
        host: (!host.is_empty()).then_some(host),
        port,
        database,
        path: None,
        params,
    })
}

/// `url` with any password replaced by `***`: `user:secret@`, password-like parameters
/// (keys compared percent-decoded), and the whole authority when it may hold a half-typed
/// `user:password` (no `@` yet, and text after its last `:` that is not a port).
///
/// An `@` past the end of the authority means the URL is invalid or its password holds a
/// `/`, `?` or `#`: everything from the first `:` up to the last `@` is redacted then, even
/// if that hides more than a password.
pub fn redact_url(url: &str) -> String {
    let mut out = url.to_string();
    if let Some(scheme_end) = out.find("://") {
        let start = scheme_end + 3;
        let end = out[start..]
            .find(['/', '?', '#'])
            .map_or(out.len(), |i| start + i);
        let last_at = out[start..].rfind('@').map(|i| start + i);
        if let Some(at) = last_at.filter(|&at| at > end) {
            if let Some(colon) = out[start..at].find(':') {
                out.replace_range(start + colon + 1..at, "***");
            }
        } else if may_hide_password(&out[start..end]) {
            out.replace_range(start..end, "***");
        } else if let Some(at) = out[start..end].rfind('@') {
            let at = start + at;
            if let Some(colon) = out[start..at].find(':') {
                out.replace_range(start + colon + 1..at, "***");
            }
        }
    }
    if let Some(q) = out.find('?') {
        let query: Vec<String> = out[q + 1..]
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((k, _))
                    if is_password_key(k)
                        || percent_decode(k).is_ok_and(|k| is_password_key(&k)) =>
                {
                    format!("{k}=***")
                }
                _ => pair.to_string(),
            })
            .collect();
        out.truncate(q + 1);
        out.push_str(&query.join("&"));
    }
    out
}

/// Lowercase without `_` and `-`, so `sslmode`, `ssl_mode` and `ssl-mode` match.
fn normalized(key: &str) -> String {
    key.chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect()
}

impl ConnectionUrl {
    /// Writes the URL's parts into the form `values` of `manifest`'s fields: `host`, `port`,
    /// `database`, `user`, `path`, and each parameter whose name matches a field key
    /// (ignoring case, `_` and `-`). A choice takes the option matching the value the same way.
    pub fn apply(&self, manifest: &DriverManifest, values: &mut BTreeMap<String, String>) {
        let mut set = |key: &str, value: &str| {
            let Some(field) = manifest
                .fields
                .iter()
                .find(|f| !f.secret && normalized(&f.key) == normalized(key))
            else {
                return;
            };
            let value = match &field.ty {
                FieldType::Choice(options) => {
                    match options
                        .iter()
                        .find(|(v, _)| normalized(v) == normalized(value))
                    {
                        Some((v, _)) => v.clone(),
                        None => return,
                    }
                }
                _ => value.to_string(),
            };
            values.insert(field.key.clone(), value);
        };
        if let Some(host) = &self.host {
            set("host", host);
        }
        if let Some(port) = self.port {
            set("port", &port.to_string());
        }
        if let Some(database) = &self.database {
            set("database", database);
        }
        if let Some(user) = &self.user {
            set("user", user);
        }
        if let Some(path) = &self.path {
            set("path", path);
        }
        for (key, value) in &self.params {
            let key = match normalized(key).as_str() {
                "dbname" => "database".to_string(),
                "username" => "user".to_string(),
                _ => key.clone(),
            };
            set(&key, value);
        }
    }
}

/// `text` with the characters that end a URL part percent-encoded.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '%' | '@' | ':' | '/' | '?' | '#' | '&' | '=' | ' ' | '[' | ']' => {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// The URL of a saved connection, as the form's URL box reads it back:
/// `scheme://[user@]host[:port][/database]`, or `sqlite:///path` for a file. It never holds
/// a secret (configs keep none). `None` for a driver without URL schemes.
pub fn connection_url(
    cfg: &crate::config::ConnectionConfig,
    manifest: &DriverManifest,
) -> Option<String> {
    let scheme = manifest.url_schemes.first()?;
    let value = |key: &str| {
        cfg.value(manifest, key)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    if FILE_SCHEMES.contains(&scheme.as_str()) {
        let path = value("path").unwrap_or_default();
        let slash = if path.starts_with('/') { "" } else { "/" };
        return Some(format!("{scheme}://{slash}{path}"));
    }
    let mut url = format!("{scheme}://");
    if let Some(user) = value("user") {
        url.push_str(&percent_encode(user));
        url.push('@');
    }
    let host = value("host").unwrap_or("localhost");
    if host.contains(':') {
        url.push_str(&format!("[{host}]"));
    } else {
        url.push_str(&percent_encode(host));
    }
    if let Some(port) = value("port") {
        url.push(':');
        url.push_str(port);
    }
    if let Some(database) = value("database") {
        url.push('/');
        url.push_str(&percent_encode(database));
    }
    Some(redact_url(&url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Capabilities, Family, FieldSpec, FieldType};

    #[test]
    fn connection_url_reads_back() {
        let manifest = crate::manifest::DriverManifest {
            id: "pg".into(),
            name: "PG".into(),
            family: Family::Relational,
            capabilities: Capabilities::default(),
            fields: ["host", "port", "database", "user"]
                .iter()
                .map(|k| field(k, FieldType::Text))
                .collect(),
            dialect: "pg".into(),
            url_schemes: vec!["postgresql".into()],
            default_port: Some(5432),
            editing: None,
        };
        let cfg = crate::config::ConnectionConfig {
            id: "c".into(),
            name: "n".into(),
            driver: "pg".into(),
            group: None,
            color: None,
            values: [
                ("host".to_string(), "db.example".to_string()),
                ("port".to_string(), "6543".to_string()),
                ("user".to_string(), "bob@corp".to_string()),
                ("database".to_string(), "shop".to_string()),
            ]
            .into(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        let url = connection_url(&cfg, &manifest).unwrap();
        assert_eq!(url, "postgresql://bob%40corp@db.example:6543/shop");
        let parsed = parse_connection_url(&url).unwrap();
        assert_eq!(parsed.user.as_deref(), Some("bob@corp"));
        assert_eq!(parsed.port, Some(6543));
    }

    fn params(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parse_urls() {
        let u =
            parse_connection_url("postgresql://bob@db.example:6543/shop?sslmode=require").unwrap();
        assert_eq!(u.scheme, "postgresql");
        assert_eq!(u.user.as_deref(), Some("bob"));
        assert_eq!(u.host.as_deref(), Some("db.example"));
        assert_eq!(u.port, Some(6543));
        assert_eq!(u.database.as_deref(), Some("shop"));
        assert_eq!(u.path, None);
        assert_eq!(u.params, params(&[("sslmode", "require")]));

        let u = parse_connection_url("postgres://localhost").unwrap();
        assert_eq!(u.scheme, "postgres");
        assert_eq!(
            (u.user, u.host.as_deref(), u.port, u.database),
            (None, Some("localhost"), None, None)
        );

        let u = parse_connection_url("MySQL://root@127.0.0.1:3307/app").unwrap();
        assert_eq!(u.scheme, "mysql");
        assert_eq!(u.port, Some(3307));
        assert_eq!(u.database.as_deref(), Some("app"));

        let u = parse_connection_url("mariadb://me@maria/").unwrap();
        assert_eq!(u.scheme, "mariadb");
        assert_eq!(u.database, None);

        for url in ["sqlite:/abs/path.db", "sqlite:///abs/path.db"] {
            let u = parse_connection_url(url).unwrap();
            assert_eq!(u.scheme, "sqlite");
            assert_eq!(u.path.as_deref(), Some("/abs/path.db"), "{url}");
            assert_eq!(u.host, None);
        }
        let u = parse_connection_url("sqlite:/my%20dir/a.db").unwrap();
        assert_eq!(u.path.as_deref(), Some("/my dir/a.db"));

        let u = parse_connection_url("postgresql://[::1]:5432/db").unwrap();
        assert_eq!(u.host.as_deref(), Some("::1"));
        assert_eq!(u.port, Some(5432));

        let u = parse_connection_url("postgresql://us%40er%3A1@h/d%2Fb").unwrap();
        assert_eq!(u.user.as_deref(), Some("us@er:1"));
        assert_eq!(u.database.as_deref(), Some("d/b"));

        let u = parse_connection_url("mysql://h?ssl-mode=REQUIRED&connect_timeout=5").unwrap();
        assert_eq!(
            u.params,
            params(&[("ssl-mode", "REQUIRED"), ("connect_timeout", "5")])
        );

        assert!(parse_connection_url("not a url").is_err());
        assert!(parse_connection_url("postgresql://h:99999/db").is_err());
    }

    #[test]
    fn url_with_password_refused() {
        let message = "Remove the password from the URL; enter it in the Password field";
        for url in [
            "postgresql://bob:secret@h/db",
            "mysql://u:p@[::1]:3306",
            "postgresql://h/db?password=secret",
        ] {
            assert_eq!(parse_connection_url(url).unwrap_err(), message, "{url}");
        }
    }

    #[test]
    fn api_keys_in_urls_are_refused_and_redacted() {
        for key in ["api_key", "api-key", "APIKEY", "%61pi%5Fkey"] {
            let url = format!("https://localhost:9200/?{key}=secret-api-credential");
            assert!(parse_connection_url(&url).is_err(), "accepted {key}");
            assert_eq!(
                redact_url(&url),
                format!("https://localhost:9200/?{key}=***")
            );
        }
    }

    #[test]
    fn malformed_url_errors_do_not_echo_credentials() {
        for url in [
            "postgresql://host/db?custom=secret-credential%ZZ",
            "postgresql://host/db?custom=secret-credential%FF",
            "postgresql://secret-credential%ZZ@host/db",
        ] {
            let error = parse_connection_url(url).unwrap_err();
            assert!(!error.contains("secret-credential"), "{error}");
        }
    }

    #[test]
    fn redact_hides_password() {
        assert_eq!(
            redact_url("postgresql://bob:secret@h:5432/db"),
            "postgresql://bob:***@h:5432/db"
        );
        assert_eq!(
            redact_url("mysql://h/db?user=a&password=pw"),
            "mysql://h/db?user=a&password=***"
        );
        assert_eq!(redact_url("postgresql://bob@h/db"), "postgresql://bob@h/db");
        assert_eq!(redact_url("sqlite:/a.db"), "sqlite:/a.db");
    }

    #[test]
    fn partial_password_never_leaks() {
        // Typing `user:password` before the `@` looks like a host and a port.
        let err = parse_connection_url("postgresql://bob:secr").unwrap_err();
        assert!(!err.contains("secr"), "{err}");
        assert_eq!(redact_url("postgresql://bob:secr"), "postgresql://***");
        assert_eq!(
            redact_url("postgresql://bob:secr/db"),
            "postgresql://***/db"
        );
        assert_eq!(
            redact_url("postgresql://host:5432/db"),
            "postgresql://host:5432/db"
        );
        assert_eq!(redact_url("postgresql://host:/db"), "postgresql://host:/db");
        assert!(
            parse_connection_url("postgresql://host:54a2/db").unwrap_err()
                == "Invalid port: it must be a number from 1 to 65535"
        );
        for key in [
            "sslpassword",
            "db_passwd",
            "PWD",
            "client_secret",
            "access_token",
        ] {
            let url = format!("postgresql://h/db?{key}=hunter2");
            assert_eq!(
                parse_connection_url(&url).unwrap_err(),
                "Remove the password from the URL; enter it in the Password field",
                "{key}"
            );
            assert!(!redact_url(&url).contains("hunter2"), "{key}");
        }
    }

    #[test]
    fn redact_hides_percent_encoded_password_keys() {
        // `parse_params` decodes the key before refusing it, so `redact_url` must decode
        // it too or the error path leaves the password in plain sight.
        let url = "postgresql://h/db?pass%77ord=hunter2";
        assert_eq!(parse_connection_url(url).unwrap_err(), PASSWORD_REFUSED);
        let redacted = redact_url(url);
        assert!(!redacted.contains("hunter2"), "{redacted}");
    }

    #[test]
    fn redact_hides_password_suffixes_after_url_delimiters() {
        // A password containing `/` or `?` is not a valid URL, but the error path still
        // echoes the redacted text. Everything up to the last `@` may be a password.
        for url in ["postgresql://bob:pa/ss@h/db", "postgresql://bob:pa?ss@h/db"] {
            let redacted = redact_url(url);
            assert!(!redacted.contains("ss@"), "{url} -> {redacted}");
        }
    }

    #[test]
    fn fragment_is_not_part_of_a_query_value() {
        let u = parse_connection_url("postgresql://h/db?sslmode=require#frag").unwrap();
        assert_eq!(u.params, params(&[("sslmode", "require")]));
    }

    #[test]
    fn sqlite_url_with_a_host_but_no_path_keeps_the_file_name() {
        // `sqlite://foo.db` is not a SQLite URL: the authority is `foo.db` and the path is
        // empty, so either refuse it or use the name as the file.
        if let Ok(u) = parse_connection_url("sqlite://foo.db") {
            assert!(u.path.is_some(), "the file name was dropped: {u:?}");
        }
    }

    #[test]
    fn unbracketed_ipv6_is_not_split_at_the_last_colon() {
        // `::1` is an IPv6 address, not host `:` with port 1. Accept only a correct
        // parse or a refusal.
        match parse_connection_url("postgresql://::1/db") {
            Err(_) => {}
            Ok(u) => assert!(u.host.as_deref() != Some(":") || u.port != Some(1), "{u:?}"),
        }
    }

    #[test]
    fn invalid_urls_are_refused_without_echoing_secrets() {
        for url in [
            "postgresql://bob:pa/ss@h/db",
            "postgresql://bob:pa?ss@h/db",
            "postgresql://bob:pa#ss@h/db",
            "postgresql://bob:pa:ss/db",
            "postgresql://::1/db",
        ] {
            let err = parse_connection_url(url).unwrap_err();
            for secret in ["pa/ss", "pa?ss", "pa#ss", "pa:ss", "bob:"] {
                assert!(!err.contains(secret), "{url}: {err}");
            }
        }
        assert_eq!(
            redact_url("postgresql://bob:pa/ss@h/db"),
            "postgresql://bob:***@h/db"
        );
        assert_eq!(
            redact_url("postgresql://h/db?Pass%57ord=x&a=1"),
            "postgresql://h/db?Pass%57ord=***&a=1"
        );
    }

    #[test]
    fn ipv6_hosts_need_brackets() {
        assert!(parse_connection_url("postgresql://::1:5432/db").is_err());
        let u = parse_connection_url("postgresql://[fe80::1]/db").unwrap();
        assert_eq!((u.host.as_deref(), u.port), (Some("fe80::1"), None));
    }

    #[test]
    fn sqlite_urls_name_local_files_only() {
        let u = parse_connection_url("sqlite://localhost/abs/a.db").unwrap();
        assert_eq!(u.path.as_deref(), Some("/abs/a.db"));
        for url in ["sqlite://foo.db", "sqlite://host/abs/a.db"] {
            let err = parse_connection_url(url).unwrap_err();
            assert!(err.contains("sqlite:///path/to/file.db"), "{url}: {err}");
        }
        let u = parse_connection_url("sqlite:///a.db?mode=ro#x").unwrap();
        assert_eq!(u.path.as_deref(), Some("/a.db"));
        assert_eq!(u.params, params(&[("mode", "ro")]));
    }

    fn field(key: &str, ty: FieldType) -> FieldSpec {
        FieldSpec {
            free_form: false,
            key: key.into(),
            label: key.into(),
            ty,
            default: None,
            required: false,
            secret: false,
            group: "Main".into(),
        }
    }

    #[test]
    fn apply_fills_matching_fields() {
        let manifest = crate::manifest::DriverManifest {
            id: "pg".into(),
            name: "PG".into(),
            family: Family::Relational,
            capabilities: Capabilities::default(),
            fields: vec![
                field("host", FieldType::Text),
                field("port", FieldType::Port),
                field("database", FieldType::Text),
                field("user", FieldType::Text),
                field(
                    "ssl_mode",
                    FieldType::Choice(vec![
                        ("disable".into(), "Disable".into()),
                        ("verify_full".into(), "Verify full".into()),
                    ]),
                ),
            ],
            dialect: "pg".into(),
            url_schemes: vec!["postgresql".into()],
            default_port: Some(5432),
            editing: None,
        };
        let mut values = std::collections::BTreeMap::new();
        values.insert("host".to_string(), "old".to_string());
        parse_connection_url("postgresql://bob@db:6543/shop?sslmode=VERIFY-FULL&x=1")
            .unwrap()
            .apply(&manifest, &mut values);
        assert_eq!(values["host"], "db");
        assert_eq!(values["port"], "6543");
        assert_eq!(values["database"], "shop");
        assert_eq!(values["user"], "bob");
        assert_eq!(values["ssl_mode"], "verify_full");
        assert!(!values.contains_key("x"));
    }
}
