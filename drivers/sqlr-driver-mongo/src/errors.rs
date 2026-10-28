//! Mapping from `mongodb` errors and server error codes to the shared `DriverError`.

use mongodb::error::{Error, ErrorKind as MongoKind, WriteFailure};
use sqlr_core::{DriverError, ErrorKind};

/// The kind for a server error code (`src/mongo/base/error_codes.yml`).
pub(crate) fn kind_for_code(code: i32) -> ErrorKind {
    match code {
        18 => ErrorKind::Auth,
        13 | 31 => ErrorKind::Permission,
        // Interrupted (killOp), cursor killed.
        11601 | 237 => ErrorKind::Cancelled,
        50 | 202 | 262 => ErrorKind::Timeout,
        6 | 7 | 89 | 91 | 189 | 11600 | 11602 => ErrorKind::Connection,
        2 | 9 | 14 | 59 | 168 | 40324 | 40323 | 17276 | 15983 => ErrorKind::Syntax,
        _ => ErrorKind::Other,
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` function pointer, which receives the error by value"
)]
pub(crate) fn map_error(e: Error) -> DriverError {
    match &*e.kind {
        MongoKind::Authentication { message, .. } => {
            let message = if message.contains("Authentication failed") {
                message.clone()
            } else {
                format!("Authentication failed: {message}")
            };
            DriverError::new(ErrorKind::Auth, message)
        }
        MongoKind::Command(c) => {
            let kind = kind_for_code(c.code);
            let message = if kind == ErrorKind::Cancelled {
                format!("The operation was cancelled ({})", c.message)
            } else {
                c.message.clone()
            };
            DriverError::new(kind, message)
        }
        MongoKind::Write(WriteFailure::WriteError(w)) => {
            DriverError::new(kind_for_code(w.code), w.message.clone())
        }
        MongoKind::Write(WriteFailure::WriteConcernError(w)) => DriverError::new(
            ErrorKind::Other,
            format!("write concern error: {}", w.message),
        ),
        MongoKind::ServerSelection { message, .. } => DriverError::new(
            ErrorKind::Connection,
            format!("Could not reach the MongoDB server: {message}"),
        ),
        MongoKind::Io(io) => DriverError::new(ErrorKind::Connection, io.to_string()),
        MongoKind::DnsResolve { message, .. } | MongoKind::InvalidTlsConfig { message, .. } => {
            DriverError::new(ErrorKind::Connection, message.clone())
        }
        MongoKind::ConnectionPoolCleared { message, .. } => {
            DriverError::new(ErrorKind::Connection, message.clone())
        }
        MongoKind::InvalidArgument { message, .. } => {
            DriverError::new(ErrorKind::Syntax, message.clone())
        }
        MongoKind::InvalidResponse { message, .. } => {
            DriverError::new(ErrorKind::Protocol, message.clone())
        }
        MongoKind::BsonDeserialization(_)
        | MongoKind::BsonSerialization(_)
        | MongoKind::Bson(_) => DriverError::new(ErrorKind::Protocol, e.to_string()),
        _ => DriverError::new(ErrorKind::Other, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_map_to_kinds() {
        assert_eq!(kind_for_code(18), ErrorKind::Auth);
        assert_eq!(kind_for_code(13), ErrorKind::Permission);
        assert_eq!(kind_for_code(11601), ErrorKind::Cancelled);
        assert_eq!(kind_for_code(50), ErrorKind::Timeout);
        assert_eq!(kind_for_code(59), ErrorKind::Syntax);
        assert_eq!(kind_for_code(11000), ErrorKind::Other);
    }
}
