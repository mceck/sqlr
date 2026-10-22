//! The Driver Manager and the install prompts: the window that lists,
//! installs, updates and removes drivers, the review step of an install from a file, and
//! the "Install" a missing driver offers wherever it is needed.

mod install_prompt;
mod manager;

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use sqlr_ui::DriverManagerState;

/// An open file dialog for "Install from file…".
pub(crate) struct ManualPick {
    file: Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>>,
}

impl ManualPick {
    pub(crate) fn open() -> Self {
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("Choose a driver archive")
            .add_filter("Driver archive", &["gz", "tgz"]);
        Self {
            file: Box::pin(dialog.pick_file()),
        }
    }

    /// `Some(Some(path))` once a file was chosen, `Some(None)` when dismissed.
    pub(crate) fn poll(&mut self) -> Option<Option<std::path::PathBuf>> {
        let mut cx = Context::from_waker(Waker::noop());
        match self.file.as_mut().poll(&mut cx) {
            Poll::Ready(file) => Some(file.map(|f| f.path().to_path_buf())),
            Poll::Pending => None,
        }
    }
}

/// An install the user asked for before the registry index was loaded: it starts when the
/// index arrives and, when it came from the new-connection catalog, continues into the
/// connection form.
pub(crate) struct PendingInstall {
    pub driver: String,
    pub known: Option<&'static str>,
    pub started: bool,
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
}
