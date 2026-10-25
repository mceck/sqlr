//! The Driver Manager and the install prompts: the window that downloads, lists and removes
//! drivers, the review step of an install from a local file, and the install a missing
//! driver offers wherever it is needed.

mod install_prompt;
mod manager;

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use sqlr_ui::DriverManagerState;

/// Whether the sqlr release publishes driver `id`: the drivers of the known databases that
/// do not ship with the app (the release workflow packages exactly these).
pub(crate) fn downloadable(id: &str) -> bool {
    sqlr_core::known_databases().iter().any(
        |db| matches!(db.status, sqlr_core::KnownStatus::Installable { driver } if driver == id),
    )
}

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
    fn the_release_packages_every_downloadable_driver() {
        let script = include_str!("../../../../scripts/package-release-drivers.sh");
        let line = script
            .lines()
            .find_map(|l| l.strip_prefix("DRIVERS=\""))
            .expect("the script lists its drivers");
        let mut packed: Vec<&str> = line.trim_end_matches('"').split_whitespace().collect();
        let mut offered: Vec<&str> = sqlr_core::known_databases()
            .iter()
            .filter_map(|db| match db.status {
                sqlr_core::KnownStatus::Installable { driver } => Some(driver),
                _ => None,
            })
            .collect();
        packed.sort_unstable();
        offered.sort_unstable();
        offered.dedup();
        assert_eq!(packed, offered);
        assert!(packed.iter().all(|id| downloadable(id)));
        assert!(!downloadable("postgres"));
    }

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
