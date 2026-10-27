//! The real PTY backend (`portable-pty`).

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::session::{BackendFactory, BackendParts, PtyError};
use crate::shell::CommandSpec;

#[derive(Default)]
pub struct PortablePty;

impl BackendFactory for PortablePty {
    fn start(&self, spec: &CommandSpec, rows: u16, cols: u16) -> Result<BackendParts, PtyError> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Backend(e.into()))?;
        let mut command = CommandBuilder::new(&spec.program);
        command.args(&spec.args);
        command.cwd(&spec.cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        // Do not leak the MCP token or any agent-specific variable into the shell.
        command.env_remove("SQLR_MCP_TOKEN");
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| PtyError::Backend(e.into()))?;
        drop(pair.slave);
        let pid = child.process_id();
        let killer = child.clone_killer();
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::Backend(e.into()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::Backend(e.into()))?;
        let master: Arc<Mutex<Box<dyn MasterPty + Send>>> = Arc::new(Mutex::new(pair.master));
        let resize_master = master.clone();
        let resize = Box::new(move |rows: u16, cols: u16| {
            let _ = resize_master.lock().resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        });
        let killer = Arc::new(Mutex::new(killer));
        let kill = Box::new(move || {
            let _ = killer.lock().kill();
            #[cfg(unix)]
            if let Some(pid) = pid {
                let pid = pid as i32;
                std::thread::spawn(move || {
                    // A short grace period, then the whole process group.
                    std::thread::sleep(Duration::from_millis(150));
                    // SAFETY: `libc::kill` takes plain integers and dereferences nothing. A
                    // negative pid addresses the process group led by the child, which we
                    // started as a session leader; a stale pid only makes the call fail
                    // with ESRCH, which is ignored.
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                });
            }
        });
        let wait = Box::new(move || {
            let status = child.wait()?;
            Ok(status.exit_code())
        });
        Ok(BackendParts {
            reader,
            writer,
            resize,
            kill,
            wait,
            pid,
        })
    }
}
