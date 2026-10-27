//! A thread-local marker the panic hook can read.
//!
//! The engine catches panics in command tasks and turns them into an `engine_panic`
//! incident. The application's global panic hook must not write a second `panic` incident
//! for the same event, so the engine marks every poll of a command future with
//! [`EngineTaskScope`]. The marker is set around each poll (not once per task) because a
//! Tokio task may resume on a different worker thread.

use std::cell::Cell;

thread_local! {
    static ENGINE_TASK: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread as polling an engine command task until dropped.
pub struct EngineTaskScope {
    previous: bool,
}

impl EngineTaskScope {
    /// Marks the current thread; the mark is undone when the returned scope is dropped.
    pub fn enter() -> Self {
        let previous = ENGINE_TASK.with(|flag| flag.replace(true));
        Self { previous }
    }
}

impl Drop for EngineTaskScope {
    fn drop(&mut self) {
        ENGINE_TASK.with(|flag| flag.set(self.previous));
    }
}

/// Whether the current thread is polling an engine command task.
pub fn in_engine_task() -> bool {
    ENGINE_TASK.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_restored() {
        assert!(!in_engine_task());
        {
            let _scope = EngineTaskScope::enter();
            assert!(in_engine_task());
            {
                let _nested = EngineTaskScope::enter();
                assert!(in_engine_task());
            }
            assert!(in_engine_task());
        }
        assert!(!in_engine_task());
    }
}
