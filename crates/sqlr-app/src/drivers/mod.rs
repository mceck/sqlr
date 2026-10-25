//! The Driver Manager and the install prompts: the window that lists and removes drivers
//! installed from local files, the review step of a file install, and the "Install from
//! file…" a missing driver offers wherever it is needed.

mod install_prompt;
mod manager;

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use sqlr_ui::DriverManagerState;

/// An open file dialog for "Install from file…", or the answer a test gave in advance.
pub(crate) struct ManualPick {
    file: Pin<Box<dyn Future<Output = Option<std::path::PathBuf>>>>,
}

impl ManualPick {
    /// The native dialog, unless `answer` holds the choice already.
    pub(crate) fn open(answer: Option<Option<std::path::PathBuf>>) -> Self {
        if let Some(answer) = answer {
            return Self {
                file: Box::pin(async move { answer }),
            };
        }
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("Choose a driver archive")
            .add_filter("Driver archive", &["gz", "tgz"]);
        let pick = dialog.pick_file();
        Self {
            file: Box::pin(async move { pick.await.map(|f| f.path().to_path_buf()) }),
        }
    }

    /// `Some(Some(path))` once a file was chosen, `Some(None)` when dismissed.
    pub(crate) fn poll(&mut self) -> Option<Option<std::path::PathBuf>> {
        let mut cx = Context::from_waker(Waker::noop());
        match self.file.as_mut().poll(&mut cx) {
            Poll::Ready(file) => Some(file),
            Poll::Pending => None,
        }
    }
}

/// An install the user asked for: it opens the file picker once and, when it came from the
/// new-connection catalog, continues into the connection form.
pub(crate) struct PendingInstall {
    pub driver: String,
    pub known: Option<&'static str>,
}

#[derive(Default)]
pub(crate) struct DriversUi {
    /// The Driver Manager window, when open.
    pub manager: Option<DriverManagerState>,
    /// Set by the settings dialog (which cannot borrow the whole app).
    pub open_requested: bool,
    pub(crate) pick: Option<ManualPick>,
    pub(crate) pending: Option<PendingInstall>,
    /// A full removal waiting for the user's confirmation.
    pub(crate) confirm_remove: Option<String>,
    /// `Engine::drivers_revision` last seen, to refresh what depends on the drivers.
    pub(crate) seen_revision: u64,
    /// The driver file a test chose in advance, in place of the native dialog.
    pub(crate) file_answer: Option<Option<std::path::PathBuf>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dismissed_file_pick_is_reported_once() {
        let mut pick = ManualPick::open(Some(None));
        assert_eq!(pick.poll(), Some(None));
    }

    #[test]
    fn a_pending_file_pick_keeps_waiting() {
        let mut pick = ManualPick {
            file: Box::pin(std::future::pending::<Option<std::path::PathBuf>>()),
        };
        assert_eq!(pick.poll(), None);
    }
}
