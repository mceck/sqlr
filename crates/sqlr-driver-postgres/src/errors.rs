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
        let mut err = DriverError::new(kind_for_sqlstate(db.code().code()), db.message());
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

/// 57P01-57P03 (admin shutdown, crash shutdown, cannot connect now) end the session like a
/// lost connection.
fn kind_for_sqlstate(code: &str) -> ErrorKind {
    if code.starts_with("28") {
        ErrorKind::Auth
    } else if code == "42501" {
        ErrorKind::Permission
    } else if code.starts_with("42") {
        ErrorKind::Syntax
    } else if code == "57014" {
        ErrorKind::Cancelled
    } else if code.starts_with("08") || ["57P01", "57P02", "57P03"].contains(&code) {
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
        for code in ["57P01", "57P02", "57P03", "08006"] {
            assert_eq!(kind_for_sqlstate(code), ErrorKind::Connection, "{code}");
        }
        assert_eq!(kind_for_sqlstate("57014"), ErrorKind::Cancelled);
        assert_eq!(kind_for_sqlstate("28P01"), ErrorKind::Auth);
        assert_eq!(kind_for_sqlstate("42501"), ErrorKind::Permission);
        assert_eq!(kind_for_sqlstate("42601"), ErrorKind::Syntax);
        assert_eq!(kind_for_sqlstate("22012"), ErrorKind::Other);
    }
}
