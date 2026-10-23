//! Logging to stderr and a daily rotating file. Never log `Secrets` or form secret values.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

/// Installs the global subscriber: stderr, plus `<log_dir>/sqlr.log.<date>` when a directory
/// is given and usable (otherwise stderr only, with a warning). The file is written by a
/// background thread, so logging never blocks the UI. Keep the returned guard alive until
/// exit; dropping it flushes the file.
pub fn init(log_dir: Option<&Path>) -> Option<WorkerGuard> {
    let filter =
        || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,rmcp=warn"));
    let stderr = fmt::layer().with_writer(std::io::stderr);
    // Under `cargo test` the harness captures it.
    #[cfg(test)]
    let stderr = stderr.with_test_writer();
    let stderr = stderr.with_filter(filter());
    let appender = log_dir.map(|dir| {
        RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix("sqlr.log")
            .build(dir)
            .map_err(|e| (dir, e))
    });
    let (file, guard, failure) = match appender {
        Some(Ok(appender)) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter());
            (Some(layer), Some(guard), None)
        }
        Some(Err(failure)) => (None, None, Some(failure)),
        None => (None, None, None),
    };
    // A second `init` (e.g. in tests) keeps the first subscriber.
    let _ = tracing_subscriber::registry()
        .with(stderr)
        .with(file)
        .try_init();
    if let Some((dir, e)) = failure {
        tracing::warn!(dir = %dir.display(), error = %e, "cannot log to file; logging to stderr only");
    }
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unusable_log_dir_falls_back_to_stderr() {
        let file = tempfile::NamedTempFile::new().unwrap();
        // A directory cannot be created beneath a regular file.
        let dir = file.path().join("logs");
        assert!(init(Some(&dir)).is_none());
    }
}
