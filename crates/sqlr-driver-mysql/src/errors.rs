//! Mapping from `mysql_async` errors to the shared `DriverError`.

use mysql_async::{DriverError as MyDriverError, Error, IoError};
use sqlr_core::{DriverError, ErrorKind};

/// Message for a server that refuses TLS when the connection requires it.
pub(crate) const NO_TLS: &str = "The server does not support TLS, but the SSL mode requires it. Set the SSL mode to \
     Preferred or Disabled, or enable TLS on the server.";

pub(crate) fn map_error(e: Error) -> DriverError {
    match e {
        Error::Server(s) => DriverError::new(kind_for_server(s.code, &s.state), s.message),
        Error::Driver(MyDriverError::NoClientSslFlagFromServer) => {
            DriverError::new(ErrorKind::Connection, NO_TLS)
        }
        Error::Driver(MyDriverError::ConnectionClosed) => DriverError::new(
            ErrorKind::Connection,
            "the connection to the server is closed",
        ),
        Error::Io(IoError::Tls(tls)) => {
            DriverError::new(ErrorKind::Connection, format!("TLS error: {}", chain(&tls)))
        }
        Error::Io(io) => DriverError::new(ErrorKind::Connection, chain(&io)),
        Error::Driver(d) => DriverError::new(ErrorKind::Protocol, d.to_string()),
        other => DriverError::new(ErrorKind::Other, chain(&other)),
    }
}

/// `e` and its sources, joined by `: `.
fn chain(e: &dyn std::error::Error) -> String {
    let mut message = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        let text = s.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = s.source();
    }
    message
}

/// Server error codes and SQLSTATEs to kinds. 1317 is "query execution was interrupted"
/// (`KILL QUERY`); 1053, 1927 (MariaDB's killed connection) and 4031 (MySQL's idle
/// disconnect) end the session like a lost connection.
fn kind_for_server(code: u16, state: &str) -> ErrorKind {
    match code {
        1045 | 1698 => ErrorKind::Auth,
        1044 | 1142 | 1143 | 1227 | 1370 => ErrorKind::Permission,
        1317 => ErrorKind::Cancelled,
        3024 | 1969 => ErrorKind::Timeout,
        1053 | 1927 | 4031 => ErrorKind::Connection,
        1064 | 1146 | 1054 | 1049 | 1305 | 1109 => ErrorKind::Syntax,
        _ if state.starts_with("28") => ErrorKind::Auth,
        _ if state.starts_with("08") => ErrorKind::Connection,
        _ if state.starts_with("42") => ErrorKind::Syntax,
        _ => ErrorKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mysql_async::ServerError;

    fn server(code: u16, state: &str) -> ErrorKind {
        map_error(Error::Server(ServerError {
            code,
            message: "m".into(),
            state: state.into(),
        }))
        .kind
    }

    #[test]
    fn server_errors_by_code_and_state() {
        assert_eq!(server(1045, "28000"), ErrorKind::Auth);
        assert_eq!(server(1142, "42000"), ErrorKind::Permission);
        assert_eq!(server(1064, "42000"), ErrorKind::Syntax);
        assert_eq!(server(1146, "42S02"), ErrorKind::Syntax);
        assert_eq!(server(1317, "70100"), ErrorKind::Cancelled);
        assert_eq!(server(3024, "HY000"), ErrorKind::Timeout);
        for code in [1053, 1927, 4031] {
            assert_eq!(server(code, "HY000"), ErrorKind::Connection, "{code}");
        }
        assert_eq!(server(1062, "23000"), ErrorKind::Other);
    }

    #[test]
    fn missing_tls_is_a_clear_connection_error() {
        let e = map_error(Error::Driver(MyDriverError::NoClientSslFlagFromServer));
        assert_eq!(e.kind, ErrorKind::Connection);
        assert!(e.message.contains("does not support TLS"), "{}", e.message);
    }
}
