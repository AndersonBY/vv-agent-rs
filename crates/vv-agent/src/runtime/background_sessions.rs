mod listeners;
mod options;
mod session;
mod subscription;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::runtime::processes::start_captured_process_with_env;

use listeners::notify_background_listeners;
use session::BackgroundSession;

pub use listeners::BackgroundSessionListener;
pub use options::{BackgroundSessionAdoptOptions, BackgroundSessionStartOptions};
pub use subscription::BackgroundSessionSubscription;

static MANAGER: OnceLock<BackgroundSessionManager> = OnceLock::new();

pub fn background_session_manager() -> &'static BackgroundSessionManager {
    MANAGER.get_or_init(BackgroundSessionManager::default)
}

#[derive(Default)]
pub struct BackgroundSessionManager {
    sessions: Mutex<BTreeMap<String, BackgroundSession>>,
    next_id: AtomicU64,
    next_listener_id: AtomicU64,
}

impl BackgroundSessionManager {
    pub fn start(
        &self,
        command: impl Into<String>,
        cwd: impl Into<PathBuf>,
        timeout_seconds: u64,
        options: BackgroundSessionStartOptions,
    ) -> Result<String, String> {
        let command = command.into();
        let cwd = cwd.into();
        let prepared = super::shell::prepare_shell_execution(
            &command,
            options.auto_confirm,
            options.stdin.as_deref(),
            options.shell.as_deref(),
            options.windows_shell_priority.as_deref(),
        )?;
        let started = start_captured_process_with_env(
            &prepared.command,
            &cwd,
            prepared.stdin.as_deref(),
            options.env.as_ref(),
        )
        .map_err(|error| error.to_string())?;
        Ok(self.adopt_running_process(
            command,
            cwd,
            timeout_seconds,
            started.child,
            started.output_path,
            prepared.shell,
        ))
    }

    pub fn adopt_running_process(
        &self,
        command: impl Into<String>,
        cwd: impl Into<PathBuf>,
        timeout_seconds: u64,
        child: std::process::Child,
        output_path: PathBuf,
        shell: Option<String>,
    ) -> String {
        let mut options =
            BackgroundSessionAdoptOptions::new(command, cwd, timeout_seconds, child, output_path);
        options.shell = shell;
        self.adopt_running_process_with_options(options)
    }

    pub fn adopt_running_process_with_options(
        &self,
        options: BackgroundSessionAdoptOptions,
    ) -> String {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let session_id = format!("bg_{id:012x}");
        let session = BackgroundSession::from_adopt_options(session_id.clone(), options);
        self.sessions
            .lock()
            .expect("background session manager poisoned")
            .insert(session_id.clone(), session);
        self.start_watch_thread(session_id.clone());
        session_id
    }

    pub fn subscribe(
        &'static self,
        session_id: &str,
        listener: BackgroundSessionListener,
    ) -> BackgroundSessionSubscription {
        let mut snapshot = None;
        let listener_id = self.next_listener_id.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let mut sessions = self
                .sessions
                .lock()
                .expect("background session manager poisoned");
            let Some(session) = sessions.get_mut(session_id) else {
                return BackgroundSessionSubscription::noop();
            };
            if session.is_terminal() {
                snapshot = Some(session.snapshot());
            } else {
                session.add_listener(listener_id, listener.clone());
            }
        }
        if let Some(payload) = snapshot {
            listener(&payload);
            return BackgroundSessionSubscription::noop();
        }
        BackgroundSessionSubscription::new(session_id.to_string(), listener_id, self)
    }

    fn unsubscribe(&self, session_id: &str, listener_id: u64) {
        let mut sessions = self
            .sessions
            .lock()
            .expect("background session manager poisoned");
        if let Some(session) = sessions.get_mut(session_id) {
            session.remove_listener(listener_id);
        }
    }

    fn start_watch_thread(&self, session_id: String) {
        let thread_name = format!("vv-agent-bg-{session_id}");
        let _ = thread::Builder::new()
            .name(thread_name)
            .spawn(move || loop {
                thread::sleep(Duration::from_millis(200));
                let payload = background_session_manager().check(&session_id);
                let status = payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("missing");
                if status != "running" {
                    break;
                }
            });
    }

    pub fn check(&self, session_id: &str) -> Value {
        let mut sessions = self
            .sessions
            .lock()
            .expect("background session manager poisoned");
        let Some(session) = sessions.get_mut(session_id) else {
            return json!({
                "status": "missing",
                "session_id": session_id,
                "error": "Background session not found",
            });
        };

        if session.is_terminal() {
            return session.snapshot();
        }

        let elapsed = session.elapsed();
        if session.timed_out(elapsed) {
            session.finalize_timeout();
            let payload = session.snapshot();
            let terminal_listeners = session.take_listeners();
            drop(sessions);
            notify_background_listeners(terminal_listeners, &payload);
            return payload;
        }

        match session.try_wait() {
            Ok(Some(exit_code)) => {
                session.finalize_completed(exit_code);
                let payload = session.snapshot();
                let terminal_listeners = session.take_listeners();
                drop(sessions);
                notify_background_listeners(terminal_listeners, &payload);
                payload
            }
            Ok(None) => session.running_snapshot(elapsed),
            Err(error) => {
                session.finalize_failed_with_output(-1, error.to_string());
                let payload = session.snapshot();
                let terminal_listeners = session.take_listeners();
                drop(sessions);
                notify_background_listeners(terminal_listeners, &payload);
                payload
            }
        }
    }
}
