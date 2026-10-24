//! Failure-line policy: which level an error kind logs at, and how SQL is capped.
//!
//! Exactly one line is written when an operation settles. Expected server errors stay log
//! lines; only `error!` is an incident, so nothing here emits `error!`.

use sqlr_core::ErrorKind;
use tracing::Level;

/// Failed SQL is recorded up to this many UTF-8 bytes.
pub const SQL_BYTES: usize = 4 * 1024;

/// Expected server errors are informational; unexpected kinds are warnings; a cancellation
/// is debug-only so pressing Stop does not clutter the log.
pub fn failure_level(kind: ErrorKind) -> Level {
    match kind {
        ErrorKind::Syntax | ErrorKind::Permission | ErrorKind::Auth | ErrorKind::Timeout => {
            Level::INFO
        }
        ErrorKind::Cancelled => Level::DEBUG,
        ErrorKind::Connection | ErrorKind::Protocol | ErrorKind::Unsupported | ErrorKind::Other => {
            Level::WARN
        }
    }
}

/// The stable name of an error kind, as the log and incident field use it.
pub fn kind_name(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Connection => "Connection",
        ErrorKind::Auth => "Auth",
        ErrorKind::Syntax => "Syntax",
        ErrorKind::Timeout => "Timeout",
        ErrorKind::Cancelled => "Cancelled",
        ErrorKind::Permission => "Permission",
        ErrorKind::Unsupported => "Unsupported",
        ErrorKind::Protocol => "Protocol",
        ErrorKind::Other => "Other",
    }
}

/// Emits a log event at a runtime-chosen level. `tracing::event!` needs a constant level,
/// so the four levels have their own macro arm.
macro_rules! log_at {
    ($level:expr, $($rest:tt)*) => {
        match $level {
            tracing::Level::ERROR => tracing::error!($($rest)*),
            tracing::Level::WARN => tracing::warn!($($rest)*),
            tracing::Level::INFO => tracing::info!($($rest)*),
            tracing::Level::TRACE => tracing::trace!($($rest)*),
            _ => tracing::debug!($($rest)*),
        }
    };
}
pub(crate) use log_at;

/// SQL as typed, capped at [`SQL_BYTES`] on a character boundary, with a marker.
pub fn truncate_sql(sql: &str) -> (String, bool) {
    if sql.len() <= SQL_BYTES {
        return (sql.to_string(), false);
    }
    let mut cut = SQL_BYTES;
    while cut > 0 && !sql.is_char_boundary(cut) {
        cut -= 1;
    }
    (format!("{}… (truncated)", &sql[..cut]), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_errors_are_not_warnings() {
        assert_eq!(failure_level(ErrorKind::Syntax), Level::INFO);
        assert_eq!(failure_level(ErrorKind::Cancelled), Level::DEBUG);
        assert_eq!(failure_level(ErrorKind::Connection), Level::WARN);
        assert_eq!(kind_name(ErrorKind::Syntax), "Syntax");
    }

    #[test]
    fn sql_truncation_keeps_characters_whole() {
        let sql = "é".repeat(SQL_BYTES);
        let (text, truncated) = truncate_sql(&sql);
        assert!(truncated);
        assert!(text.ends_with("… (truncated)"));
    }
}
