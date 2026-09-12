mod listeners;
mod options;
mod session;
mod subscription;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::runtime::processes::start_captured_process_with_env;
use crate::workspace::WorkspaceBackend;

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
    sessions: Mutex<BTreeMap<String, Arc<Mutex<BackgroundSession>>>>,
    next_listener_id: AtomicU64,
}

impl BackgroundSessionManager {
    pub fn start(
        &self,
        command: impl Into<String>,
        cwd: impl Into<PathBuf>,
        timeout_seconds: impl Into<Option<u64>>,
        options: BackgroundSessionStartOptions,
    ) -> Result<String, String> {
        let command = command.into();
        let cwd = cwd.into();
        let timeout_seconds = timeout_seconds.into();
        if timeout_seconds.is_some_and(|timeout| !(1..=86400).contains(&timeout)) {
            return Err("timeout_seconds must be an integer from 1 through 86400".to_string());
        }
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
        let mut adopted = BackgroundSessionAdoptOptions::new(
            command,
            cwd,
            timeout_seconds,
            started.child,
            started.output_path,
        )
        .with_started_at(started.started_at);
        adopted.shell = prepared.shell;
        Ok(self.adopt_running_process_with_options(adopted))
    }

    pub fn adopt_running_process(
        &self,
        command: impl Into<String>,
        cwd: impl Into<PathBuf>,
        timeout_seconds: impl Into<Option<u64>>,
        child: impl Into<super::processes::ManagedChild>,
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
        let session_id = format!("bg_{}", &Uuid::new_v4().simple().to_string()[..12]);
        let session = Arc::new(Mutex::new(BackgroundSession::from_adopt_options(
            session_id.clone(),
            options,
        )));
        self.sessions
            .lock()
            .expect("background session manager poisoned")
            .insert(session_id.clone(), session.clone());
        let _ = thread::Builder::new()
            .name(format!("vv-agent-bg-{session_id}"))
            .spawn(move || loop {
                let (terminal, listeners, payload) = {
                    let mut session = session.lock().expect("background session poisoned");
                    let listeners = session.advance();
                    (session.is_terminal(), listeners, session.snapshot())
                };
                notify_background_listeners(listeners, &payload);
                if terminal {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            });
        session_id
    }

    fn get(&self, session_id: &str) -> Option<Arc<Mutex<BackgroundSession>>> {
        self.sessions
            .lock()
            .expect("background session manager poisoned")
            .get(session_id)
            .cloned()
    }

    pub(crate) fn wait(&self, session_id: &str, yield_time_ms: u64) {
        let Some(session) = self.get(session_id) else {
            return;
        };
        loop {
            let remaining = {
                let session = session.lock().expect("background session poisoned");
                if session.is_terminal() {
                    return;
                }
                session.remaining_yield(Duration::from_millis(yield_time_ms))
            };
            if remaining.is_zero() {
                return;
            }
            thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }

    pub fn subscribe(
        &'static self,
        session_id: &str,
        listener: BackgroundSessionListener,
    ) -> BackgroundSessionSubscription {
        let Some(session) = self.get(session_id) else {
            return BackgroundSessionSubscription::noop();
        };
        let listener_id = self.next_listener_id.fetch_add(1, Ordering::Relaxed) + 1;
        let snapshot = {
            let mut session = session.lock().expect("background session poisoned");
            if session.is_terminal() {
                Some(session.snapshot())
            } else {
                session.add_listener(listener_id, listener.clone());
                None
            }
        };
        if let Some(snapshot) = snapshot {
            listener(&snapshot);
            return BackgroundSessionSubscription::noop();
        }
        BackgroundSessionSubscription::new(session_id.to_string(), listener_id, self)
    }

    fn unsubscribe(&self, session_id: &str, listener_id: u64) {
        if let Some(session) = self.get(session_id) {
            session
                .lock()
                .expect("background session poisoned")
                .remove_listener(listener_id);
        }
    }

    /// Trusted local observation. Model tools must use the owner-checked entry.
    pub fn check(&self, session_id: &str) -> Value {
        let Some(session) = self.get(session_id) else {
            return missing(session_id);
        };
        let (observation, listeners) = {
            let mut session = session.lock().expect("background session poisoned");
            let listeners = session.advance();
            (session.live_snapshot(), listeners)
        };
        let payload = observation.render();
        notify_background_listeners(listeners, &payload);
        payload
    }

    pub(crate) fn check_for_tool(
        &self,
        session_id: &str,
        backend: Arc<dyn WorkspaceBackend>,
        task_id: &str,
        call_id: &str,
        workspace: &Path,
    ) -> Value {
        self.access_for_tool(session_id, backend, task_id, call_id, workspace, false)
    }

    pub(crate) fn stop_for_tool(
        &self,
        session_id: &str,
        backend: Arc<dyn WorkspaceBackend>,
        task_id: &str,
        call_id: &str,
        workspace: &Path,
    ) -> Value {
        self.access_for_tool(session_id, backend, task_id, call_id, workspace, true)
    }

    fn access_for_tool(
        &self,
        session_id: &str,
        backend: Arc<dyn WorkspaceBackend>,
        task_id: &str,
        call_id: &str,
        workspace: &Path,
        stop: bool,
    ) -> Value {
        let Some(session) = self.get(session_id) else {
            return missing(session_id);
        };
        let (observation, listeners) = {
            let mut session = session.lock().expect("background session poisoned");
            if !session.owned_by(task_id, workspace) {
                return json!({"status": "forbidden", "session_id": session_id,
                    "error": "Background session belongs to another task or workspace", "error_code": "background_session_forbidden"});
            }
            session.set_artifact_context(backend, task_id, call_id);
            let mut listeners = session.advance();
            if stop {
                listeners.extend(session.request_stop(false));
            }
            (session.live_snapshot(), listeners)
        };
        let payload = observation.render();
        notify_background_listeners(listeners, &payload);
        payload
    }
}

fn missing(session_id: &str) -> Value {
    json!({"status": "missing", "session_id": session_id, "error": "Background session not found"})
}
