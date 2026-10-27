//! The global panic hook: the only incident producer that writes directly, because a
//! crashing process cannot rely on a background writer thread.

use std::cell::Cell;
use std::panic::PanicHookInfo;
use std::sync::Arc;

use crate::diagnostics::{Diagnostics, IncidentKind, IncidentRequest};

type Hook = dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static;

/// Keeps the previously installed hook until the process (or a test) is done.
pub struct PanicHookGuard {
    previous: Option<Arc<Hook>>,
}

impl PanicHookGuard {
    /// Installs the crash-reporting hook. It chains to whatever hook was installed before,
    /// so the standard stderr output remains.
    pub fn install(diagnostics: Arc<Diagnostics>) -> Self {
        let previous: Arc<Hook> = Arc::from(std::panic::take_hook());
        let chained = previous.clone();
        std::panic::set_hook(Box::new(move |info| {
            thread_local! {
                static IN_HOOK: Cell<bool> = const { Cell::new(false) };
            }
            let already = IN_HOOK.with(|flag| flag.replace(true));
            if !already && !sqlr_core::panic_scope::in_engine_task() {
                diagnostics.note_panic();
                let incident = panic_request(info);
                // Best effort and on this thread: the process may die next.
                let _ = diagnostics.record_now(incident);
            }
            IN_HOOK.with(|flag| flag.set(already));
            chained(info);
        }));
        Self {
            previous: Some(previous),
        }
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        // Restoring a hook while unwinding would panic again and abort the process.
        if std::thread::panicking() {
            return;
        }
        if let Some(previous) = self.previous.take() {
            std::panic::set_hook(Box::new(move |info| previous(info)));
        }
    }
}

fn panic_request(info: &PanicHookInfo<'_>) -> IncidentRequest {
    let message = payload(info);
    let location = info.location();
    let thread = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();
    let mut request = IncidentRequest::new(IncidentKind::Panic, message);
    request.target = Some("panic".into());
    request.thread = Some(thread);
    request.file = location.map(|l| l.file().to_string());
    request.line = location.map(|l| l.line());
    request
}

fn payload(info: &PanicHookInfo<'_>) -> String {
    if let Some(text) = info.payload().downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = info.payload().downcast_ref::<String>() {
        text.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{Environment, IncidentKind, ring};
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static HOOK_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn a_panic_writes_message_location_thread_session_and_logs_then_chains() {
        let _serial = HOOK_LOCK.lock();
        let dir = tempfile::tempdir().unwrap();
        let diagnostics = Arc::new(Diagnostics::open(Some(dir.path())));
        ring::global().push("the line just before the crash");
        let chained = Arc::new(AtomicUsize::new(0));
        let count = chained.clone();
        std::panic::set_hook(Box::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        }));
        let guard = PanicHookGuard::install(diagnostics.clone());

        let result = std::thread::Builder::new()
            .name("diagnostics-test-thread".into())
            .spawn(|| {
                let _ = std::panic::catch_unwind(|| panic!("assertion failed: 1 + 1"));
            })
            .unwrap()
            .join();
        assert!(result.is_ok());
        drop(guard);
        let _ = std::panic::take_hook();

        assert_eq!(chained.load(Ordering::SeqCst), 1, "chained to the old hook");
        let incidents = diagnostics.incidents();
        assert_eq!(incidents.len(), 1, "{incidents:?}");
        let incident = diagnostics.read_incident(&incidents[0].id).unwrap();
        assert_eq!(incident.kind, IncidentKind::Panic);
        assert!(incident.message.contains("assertion failed"));
        assert_eq!(incident.thread.as_deref(), Some("diagnostics-test-thread"));
        assert_eq!(incident.session_id, diagnostics.session_id());
        assert_eq!(incident.file.as_deref(), Some(file!()));
        assert!(incident.line.is_some());
        assert_eq!(incident.environment.os, Environment::basic().os);
        assert!(
            incident
                .logs
                .iter()
                .any(|l| l.contains("just before the crash"))
        );
        diagnostics.clean_exit();
        drop(diagnostics);
    }

    #[test]
    fn the_hook_guard_can_be_dropped_during_unwinding() {
        let _serial = HOOK_LOCK.lock();
        let dir = tempfile::tempdir().unwrap();
        let diagnostics = Arc::new(Diagnostics::open(Some(dir.path())));
        let original = std::panic::take_hook();
        let result = std::panic::catch_unwind({
            let diagnostics = std::panic::AssertUnwindSafe(diagnostics.clone());
            move || {
                let diagnostics = diagnostics;
                let _guard = PanicHookGuard::install(diagnostics.0);
                panic!("unwinding through the hook guard");
            }
        });
        std::panic::set_hook(original);
        assert!(result.is_err());
        assert_eq!(diagnostics.incidents().len(), 1);
    }

    #[test]
    fn an_engine_task_panic_is_left_to_the_engine_converter() {
        let _serial = HOOK_LOCK.lock();
        let dir = tempfile::tempdir().unwrap();
        let diagnostics = Arc::new(Diagnostics::open(Some(dir.path())));
        let guard = PanicHookGuard::install(diagnostics.clone());
        let _ = std::panic::catch_unwind(|| {
            let _scope = sqlr_core::panic_scope::EngineTaskScope::enter();
            panic!("engine task");
        });
        drop(guard);
        let _ = std::panic::take_hook();
        assert!(
            diagnostics.incidents().is_empty(),
            "the engine writes engine_panic instead"
        );
        diagnostics.clean_exit();
    }
}
