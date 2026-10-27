//! Mapping from `rusqlite` errors to the shared `DriverError`.

use rusqlite::ErrorCode;
use sqlr_core::{DriverError, ErrorKind};

#[expect(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` function pointer, which receives the error by value"
)]
pub(crate) fn map_error(e: rusqlite::Error) -> DriverError {
    match &e {
        rusqlite::Error::SqlInputError {
            error,
            msg,
            sql,
            offset,
            ..
        } => {
            let mut err = DriverError::new(kind_for(error.code, msg), msg.clone());
            if let Ok(byte) = usize::try_from(*offset)
                && let Some(prefix) = sql.get(..byte)
            {
                err = err.with_position(prefix.chars().count());
            }
            err
        }
        rusqlite::Error::SqliteFailure(error, msg) => {
            let message = msg.clone().unwrap_or_else(|| error.to_string());
            DriverError::new(kind_for(error.code, &message), message)
        }
        rusqlite::Error::InvalidParameterCount(..) | rusqlite::Error::MultipleStatement => {
            DriverError::new(ErrorKind::Other, e.to_string())
        }
        _ => DriverError::new(ErrorKind::Other, e.to_string()),
    }
}

fn kind_for(code: ErrorCode, message: &str) -> ErrorKind {
    match code {
        ErrorCode::OperationInterrupted => ErrorKind::Cancelled,
        ErrorCode::ReadOnly
        | ErrorCode::PermissionDenied
        | ErrorCode::AuthorizationForStatementDenied => ErrorKind::Permission,
        ErrorCode::CannotOpen | ErrorCode::NotADatabase | ErrorCode::SystemIoFailure => {
            ErrorKind::Connection
        }
        ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => ErrorKind::Timeout,
        ErrorCode::Unknown
            if message.contains("syntax error")
                || message.starts_with("no such")
                || message.contains("incomplete input") =>
        {
            ErrorKind::Syntax
        }
        // `PRAGMA query_only` reports a write attempt as a plain error.
        ErrorCode::Unknown if message.contains("readonly") => ErrorKind::Permission,
        _ => ErrorKind::Other,
    }
}
