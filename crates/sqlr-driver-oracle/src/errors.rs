//! Oracle failures mapped to the error kinds the engine and UI understand.

use oracle_rs::Error;
use sqlr_core::{DriverError, ErrorKind};

#[expect(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` function pointer, which receives the error by value"
)]
pub(crate) fn map_error(error: Error) -> DriverError {
    let kind = match &error {
        Error::InvalidCredentials | Error::AuthenticationFailed(_) => ErrorKind::Auth,
        Error::ConnectionRefused { .. }
        | Error::ConnectionClosed
        | Error::ConnectionClosedByServer(_)
        | Error::ConnectionRedirected { .. }
        | Error::ConnectionRedirect(_)
        | Error::ConnectionNotReady
        | Error::InvalidServiceName { .. }
        | Error::InvalidSid { .. } => ErrorKind::Connection,
        Error::ConnectionTimeout(_) => ErrorKind::Timeout,
        Error::Io(_) => ErrorKind::Connection,
        Error::FeatureNotSupported(_) | Error::NativeNetworkEncryptionRequired => {
            ErrorKind::Unsupported
        }
        Error::OracleError { code, .. } | Error::ServerError { code, .. } => match code {
            1017 | 28000 | 28001 => ErrorKind::Auth,
            54 | 4021 | 30036 => ErrorKind::Timeout,
            1031 | 1033 | 1045 | 1046 | 1071 => ErrorKind::Permission,
            // The session is gone (killed, not logged on, idle time exceeded, end of file
            // or lost contact on the channel, instance down): only a reconnect helps.
            28 | 1012 | 1034 | 1089 | 1092 | 2396 | 3113 | 3114 | 3135 | 12537 | 12547 => {
                ErrorKind::Connection
            }
            900..=1099 => ErrorKind::Syntax,
            12154 | 12505 | 12514 | 12541 | 12545 => ErrorKind::Connection,
            _ => ErrorKind::Other,
        },
        _ => ErrorKind::Other,
    };
    DriverError::new(kind, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_codes_map_to_kinds() {
        assert_eq!(
            map_error(Error::OracleError {
                code: 942,
                message: "table or view does not exist".into()
            })
            .kind,
            ErrorKind::Syntax
        );
        assert_eq!(
            map_error(Error::OracleError {
                code: 1017,
                message: "invalid user".into()
            })
            .kind,
            ErrorKind::Auth
        );
        assert_eq!(
            map_error(Error::FeatureNotSupported("x".into())).kind,
            ErrorKind::Unsupported
        );
    }

    fn kind(error: Error) -> ErrorKind {
        map_error(error).kind
    }

    #[test]
    fn a_session_the_server_ended_is_a_lost_connection() {
        for code in [
            28, 1012, 1034, 1089, 1092, 2396, 3113, 3114, 3135, 12537, 12547,
        ] {
            let error = Error::OracleError {
                code,
                message: "session ended".into(),
            };
            assert_eq!(kind(error), ErrorKind::Connection, "ORA-{code:05}");
        }
        let still_syntax = Error::OracleError {
            code: 904,
            message: "invalid identifier".into(),
        };
        assert_eq!(kind(still_syntax), ErrorKind::Syntax);
    }

    #[test]
    fn every_error_family_maps_to_a_kind() {
        use std::time::Duration;
        assert_eq!(kind(Error::InvalidCredentials), ErrorKind::Auth);
        assert_eq!(
            kind(Error::AuthenticationFailed("x".into())),
            ErrorKind::Auth
        );
        assert_eq!(kind(Error::ConnectionClosed), ErrorKind::Connection);
        assert_eq!(kind(Error::ConnectionNotReady), ErrorKind::Connection);
        assert_eq!(
            kind(Error::ConnectionClosedByServer("x".into())),
            ErrorKind::Connection
        );
        assert_eq!(
            kind(Error::ConnectionRedirect("h".into())),
            ErrorKind::Connection
        );
        assert_eq!(
            kind(Error::ConnectionRefused {
                error_code: None,
                message: None
            }),
            ErrorKind::Connection
        );
        assert_eq!(
            kind(Error::InvalidSid {
                sid: None,
                message: None
            }),
            ErrorKind::Connection
        );
        assert_eq!(
            kind(Error::ConnectionTimeout(Duration::from_secs(1))),
            ErrorKind::Timeout
        );
        assert_eq!(
            kind(Error::Io(std::io::Error::other("boom"))),
            ErrorKind::Connection
        );
        assert_eq!(
            kind(Error::NativeNetworkEncryptionRequired),
            ErrorKind::Unsupported
        );
        assert_eq!(kind(Error::CursorClosed), ErrorKind::Other);
    }

    #[test]
    fn server_codes_map_to_kinds() {
        let server = |code| {
            kind(Error::ServerError {
                code,
                message: "m".into(),
            })
        };
        assert_eq!(server(28000), ErrorKind::Auth);
        assert_eq!(server(54), ErrorKind::Timeout);
        assert_eq!(server(1031), ErrorKind::Permission);
        assert_eq!(server(923), ErrorKind::Syntax);
        assert_eq!(server(12541), ErrorKind::Connection);
        assert_eq!(server(60), ErrorKind::Other);
    }
}
