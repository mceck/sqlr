//! SQL Server failures mapped to the error kinds the engine and UI understand.

use sqlr_core::{DriverError, ErrorKind};

/// The server error number in a tiberius message (`… (code: 102, state: 1, class: 15)`).
fn code(message: &str) -> Option<u32> {
    let rest = message.split("(code: ").nth(1)?;
    rest.split(',').next()?.trim().parse().ok()
}

pub(crate) fn map_error(error: impl std::fmt::Display) -> DriverError {
    let message = error.to_string();
    let kind = match code(&message) {
        Some(18456 | 18452 | 4060 | 18470) => ErrorKind::Auth,
        Some(229 | 230 | 262 | 300) => ErrorKind::Permission,
        Some(1222 | 8645) => ErrorKind::Timeout,
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
}
