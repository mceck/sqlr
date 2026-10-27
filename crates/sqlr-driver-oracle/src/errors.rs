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
