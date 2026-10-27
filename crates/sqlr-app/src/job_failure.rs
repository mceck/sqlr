//! Why a background job did not finish, as the dialogs that run jobs show it.

use sqlr_core::DriverError;

/// A job that failed, was cancelled or vanished; the text is what the dialog shows.
#[derive(Debug, thiserror::Error)]
pub(crate) enum JobFailure {
    /// The job's own error.
    #[error(transparent)]
    Failed(#[from] DriverError),
    /// The person cancelled the job.
    #[error("Cancelled")]
    Cancelled,
    /// The engine no longer knows the job.
    #[error("The job disappeared")]
    Disappeared,
}

#[cfg(test)]
mod tests {
    use sqlr_core::ErrorKind;

    use super::*;

    #[test]
    fn messages_are_what_the_dialogs_show() {
        let failed = JobFailure::from(DriverError::new(ErrorKind::Other, "boom"));
        assert_eq!(failed.to_string(), "boom");
        assert_eq!(JobFailure::Cancelled.to_string(), "Cancelled");
        assert_eq!(JobFailure::Disappeared.to_string(), "The job disappeared");
    }
}
