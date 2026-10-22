//! HANA errors as `DriverError`s.

use hdbconnect_async::HdbError;
use sqlr_driver_sdk::{DriverError, ErrorKind};

/// "current operation cancelled by request and transaction rolled back".
pub(crate) const CANCELLED: i32 = 139;
/// "authentication failed".
const AUTH_FAILED: i32 = 10;
/// "sql syntax error".
const SYNTAX: i32 = 257;
/// "insufficient privilege".
const PRIVILEGE: i32 = 258;
/// "invalid user name or password" variants some versions report.
const INVALID_LOGIN: i32 = 414;

pub(crate) fn kind_of(code: i32) -> ErrorKind {
    match code {
        CANCELLED => ErrorKind::Cancelled,
        AUTH_FAILED | INVALID_LOGIN => ErrorKind::Auth,
        SYNTAX => ErrorKind::Syntax,
        PRIVILEGE => ErrorKind::Permission,
        _ => ErrorKind::Other,
    }
}

pub(crate) fn map(e: HdbError) -> DriverError {
    if let Some(server) = e.server_error() {
        let mut error = DriverError::new(
            kind_of(server.code()),
            format!("{} (HANA error {})", server.text(), server.code()),
        );
        if server.code() == SYNTAX && server.position() > 0 {
            error = error.with_position(server.position() as usize);
        }
        return error;
    }
    DriverError::new(ErrorKind::Connection, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_codes_map_to_kinds() {
        assert_eq!(kind_of(139), ErrorKind::Cancelled);
        assert_eq!(kind_of(10), ErrorKind::Auth);
        assert_eq!(kind_of(257), ErrorKind::Syntax);
        assert_eq!(kind_of(258), ErrorKind::Permission);
        assert_eq!(kind_of(1), ErrorKind::Other);
    }
}
