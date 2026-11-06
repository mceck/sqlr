//! Mapping from `tokio_postgres` errors to the shared `DriverError`.

use std::error::Error as _;

use sqlr_core::{DriverError, ErrorKind};
use tokio_postgres::error::ErrorPosition;

#[expect(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` function pointer, which receives the error by value"
)]
pub(crate) fn map_error(e: tokio_postgres::Error) -> DriverError {
    if let Some(db) = e.as_db_error() {
        let mut err = DriverError::new(
            kind_for_sqlstate(db.code().code(), db.message()),
            db.message(),
        );
        if let Some(ErrorPosition::Original(p)) = db.position() {
            err = err.with_position((*p as usize).saturating_sub(1));
        }
        if let Some(hint) = db.hint() {
            err = err.with_hint(hint);
        }
        return err;
    }

    let mut message = e.to_string();
    let mut is_io = false;
    let mut source = e.source();
    while let Some(s) = source {
        message.push_str(": ");
        message.push_str(&s.to_string());
        is_io |= s.is::<std::io::Error>();
        source = s.source();
    }
    let kind = if e.is_closed() || is_io {
        ErrorKind::Connection
    } else {
        ErrorKind::Other
    };
    DriverError::new(kind, message)
}

/// 57P01-57P05 (admin shutdown, crash shutdown, cannot connect now, database dropped, idle
/// session timeout) and 25P03 (idle in transaction timeout) end the session like a lost
/// connection.
fn kind_for_sqlstate(code: &str, message: &str) -> ErrorKind {
    if code.starts_with("28") {
        ErrorKind::Auth
    } else if code == "42501" {
        ErrorKind::Permission
    } else if code.starts_with("42") {
        ErrorKind::Syntax
    } else if code == "57014" {
        // The same state covers a user/service cancel and the server's statement_timeout.
        if message.contains("statement timeout") {
            ErrorKind::Timeout
        } else {
            ErrorKind::Cancelled
        }
    } else if code.starts_with("08")
        || ["57P01", "57P02", "57P03", "57P04", "57P05", "25P03"].contains(&code)
    {
        ErrorKind::Connection
    } else {
        ErrorKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_shutdown_and_termination_are_connection_errors() {
        for code in [
            "57P01", "57P02", "57P03", "57P04", "57P05", "25P03", "08006",
        ] {
            assert_eq!(kind_for_sqlstate(code, ""), ErrorKind::Connection, "{code}");
        }
        assert_eq!(
            kind_for_sqlstate("57014", "canceling statement due to user request"),
            ErrorKind::Cancelled
        );
        assert_eq!(
            kind_for_sqlstate("57014", "canceling statement due to statement timeout"),
            ErrorKind::Timeout,
            "a server-side statement timeout is not a user cancel"
        );
        assert_eq!(kind_for_sqlstate("28P01", ""), ErrorKind::Auth);
        assert_eq!(kind_for_sqlstate("42501", ""), ErrorKind::Permission);
        assert_eq!(kind_for_sqlstate("42601", ""), ErrorKind::Syntax);
        assert_eq!(kind_for_sqlstate("22012", ""), ErrorKind::Other);
    }
}
