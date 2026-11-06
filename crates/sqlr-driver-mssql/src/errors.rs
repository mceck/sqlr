//! SQL Server failures mapped to the error kinds the engine and UI understand.

use sqlr_core::{DriverError, ErrorKind};

/// The server error number in a tiberius message (`… (code: 102, state: 1, class: 15)`).
fn code(message: &str) -> Option<u32> {
    let rest = message.split("(code: ").nth(1)?;
    rest.split(',').next()?.trim().parse().ok()
}

pub(crate) fn map_error(error: impl std::fmt::Display) -> DriverError {
    let message = error.to_string();
    // tiberius's I/O and protocol errors (a reset, a broken pipe, the end of the stream
    // after `KILL`) leave the client unusable: the session must reconnect.
    if message.starts_with("An error occurred during the attempt of performing I/O")
        || message.starts_with("Protocol error")
    {
        return DriverError::new(ErrorKind::Connection, message);
    }
    let kind = match code(&message) {
        Some(18456 | 18452 | 4060 | 18470) => ErrorKind::Auth,
        Some(229 | 230 | 262 | 300) => ErrorKind::Permission,
        Some(1222 | 8645) => ErrorKind::Timeout,
        // The session is being killed, or the server shuts down: nothing runs on it again.
        Some(596 | 6005) => ErrorKind::Connection,
        Some(
            102 | 103 | 105 | 106 | 107 | 108 | 109 | 110 | 111 | 112 | 113 | 155 | 156 | 170 | 174
            | 319 | 1038 | 4104 | 8114 | 8156,
        ) => ErrorKind::Syntax,
        Some(_) => ErrorKind::Other,
        None => {
            let lower = message.to_ascii_lowercase();
            if lower.contains("tls") || lower.contains("certificate") {
                ErrorKind::Connection
            } else if lower.contains("timed out") || lower.contains("timeout") {
                ErrorKind::Timeout
            } else if lower.contains("connection") || lower.contains("network") {
                ErrorKind::Connection
            } else if lower.contains("login failed") {
                ErrorKind::Auth
            } else {
                ErrorKind::Other
            }
        }
    };
    DriverError::new(kind, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_codes_and_message_fallbacks() {
        assert_eq!(
            map_error("Token error: 'bad' (code: 102, state: 1, class: 15)").kind,
            ErrorKind::Syntax
        );
        assert_eq!(
            map_error("Token error: 'no' (code: 18456, state: 1, class: 14)").kind,
            ErrorKind::Auth
        );
        assert_eq!(map_error("Login failed for user").kind, ErrorKind::Auth);
        assert_eq!(map_error("connection timed out").kind, ErrorKind::Timeout);
    }

    #[test]
    fn a_broken_client_is_a_lost_connection() {
        for error in [
            tiberius::error::Error::Io {
                kind: std::io::ErrorKind::BrokenPipe,
                message: "Broken pipe (os error 32)".into(),
            },
            tiberius::error::Error::Io {
                kind: std::io::ErrorKind::UnexpectedEof,
                message: "unexpected end of file".into(),
            },
            tiberius::error::Error::Io {
                kind: std::io::ErrorKind::TimedOut,
                message: "Connection timed out (os error 110)".into(),
            },
            tiberius::error::Error::Protocol("invalid token".into()),
        ] {
            assert_eq!(map_error(&error).kind, ErrorKind::Connection, "{error}");
        }
        assert_eq!(
            map_error(
                "Token error: 'Cannot continue the execution because the session is in the kill state.' (code: 596, state: 1, class: 21)"
            )
            .kind,
            ErrorKind::Connection
        );
    }
}
