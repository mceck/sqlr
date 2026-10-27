//! One worker per used grid, with one running job and one replaceable pending job.
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

use sqlr_core::RowBuffer;

use super::sort::{self, ViewKey};

pub(super) const BACKGROUND_SORT_ROWS: usize = 10_000;

#[derive(Default)]
struct State {
    pending: Option<(ViewKey, RowBuffer)>,
    result: Option<(ViewKey, Vec<usize>)>,
    stopped: bool,
}

pub(super) struct SortWorker(Arc<(Mutex<State>, Condvar)>);

impl SortWorker {
    pub fn new(repaint: Option<egui::Context>) -> Self {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let thread = shared.clone();
        std::thread::Builder::new()
            .name("sqlr-grid-sort".into())
            .spawn(move || {
                let (lock, wake) = &*thread;
                loop {
                    let job = {
                        let mut state = lock.lock();
                        loop {
                            if state.stopped {
                                return;
                            }
                            if let Some(job) = state.pending.take() {
                                break job;
                            }
                            wake.wait(&mut state);
                        }
                    };
                    let (key, rows) = job;
                    let order = sort::compute(&rows, &key.sort, true, &key.inserted);
                    let mut state = lock.lock();
                    if state.stopped {
                        return;
                    }
                    if state.pending.is_none() {
                        state.result = Some((key, order));
                        drop(state);
                        if let Some(ctx) = &repaint {
                            ctx.request_repaint();
                        }
                    }
                }
            })
            .expect("spawn grid sort worker");
        Self(shared)
    }

    pub fn request(&self, key: ViewKey, rows: RowBuffer) {
        let mut state = self.0.0.lock();
        state.result = None;
        state.pending = Some((key, rows));
        self.0.1.notify_one();
    }

    pub fn take(&self) -> Option<(ViewKey, Vec<usize>)> {
        self.0.0.lock().result.take()
    }

    pub fn clear(&self) {
        let mut state = self.0.0.lock();
        state.pending = None;
        state.result = None;
    }
}

impl Drop for SortWorker {
    fn drop(&mut self) {
        let mut state = self.0.0.lock();
        state.stopped = true;
        state.pending = None;
        state.result = None;
        self.0.1.notify_one();
    }
}
