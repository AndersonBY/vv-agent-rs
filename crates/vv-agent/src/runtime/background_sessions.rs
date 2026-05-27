use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::processes::{
    kill_process_tree, read_captured_output, remove_captured_output,
    start_captured_process_with_env,
};

const OUTPUT_LIMIT: usize = 50_000;

static MANAGER: OnceLock<BackgroundSessionManager> = OnceLock::new();
pub type BackgroundSessionListener = Arc<dyn Fn(&Value) + Send + Sync + 'static>;

pub fn background_session_manager() -> &'static BackgroundSessionManager {
    MANAGER.get_or_init(BackgroundSessionManager::default)
}

#[derive(Debug, Clone, Default)]
pub struct BackgroundSessionStartOptions {
    pub stdin: Option<String>,
    pub auto_confirm: bool,
    pub shell: Option<String>,
    pub windows_shell_priority: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
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
        child: Child,
        output_path: PathBuf,
        shell: Option<String>,
    ) -> String {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let session_id = format!("bg_{id:012x}");
        let session = BackgroundSession {
            session_id: session_id.clone(),
            command: command.into(),
            shell,
            cwd: cwd.into(),
            started_at: Instant::now(),
            timeout_seconds: timeout_seconds.max(1),
            child: Some(child),
            output_path,
            status: BackgroundStatus::Running,
            output: String::new(),
            exit_code: None,
            listeners: BTreeMap::new(),
        };
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
            if session.status.is_terminal() {
                snapshot = Some(session.snapshot());
            } else {
                session.listeners.insert(listener_id, listener.clone());
            }
        }
        if let Some(payload) = snapshot {
            listener(&payload);
            return BackgroundSessionSubscription::noop();
        }
        BackgroundSessionSubscription {
            session_id: session_id.to_string(),
            listener_id: Some(listener_id),
            manager: self,
        }
    }

    fn unsubscribe(&self, session_id: &str, listener_id: u64) {
        let mut sessions = self
            .sessions
            .lock()
            .expect("background session manager poisoned");
        if let Some(session) = sessions.get_mut(session_id) {
            session.listeners.remove(&listener_id);
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

        if session.status.is_terminal() {
            return session.snapshot();
        }

        let elapsed = session.started_at.elapsed();
        if elapsed > Duration::from_secs(session.timeout_seconds) {
            session.finalize_timeout();
            let payload = session.snapshot();
            let terminal_listeners = session.take_listeners();
            drop(sessions);
            notify_background_listeners(terminal_listeners, &payload);
            return payload;
        }

        let Some(child) = session.child.as_mut() else {
            session.finalize_completed(0);
            return session.snapshot();
        };

        match child.try_wait() {
            Ok(Some(exit_status)) => {
                session.finalize_completed(exit_status.code().unwrap_or(-1));
                let payload = session.snapshot();
                let terminal_listeners = session.take_listeners();
                drop(sessions);
                notify_background_listeners(terminal_listeners, &payload);
                payload
            }
            Ok(None) => session.running_snapshot(elapsed),
            Err(error) => {
                session.status = BackgroundStatus::Failed;
                session.exit_code = Some(-1);
                session.output = error.to_string();
                let payload = session.snapshot();
                let terminal_listeners = session.take_listeners();
                drop(sessions);
                notify_background_listeners(terminal_listeners, &payload);
                payload
            }
        }
    }
}

pub struct BackgroundSessionSubscription {
    session_id: String,
    listener_id: Option<u64>,
    manager: &'static BackgroundSessionManager,
}

impl BackgroundSessionSubscription {
    fn noop() -> Self {
        Self {
            session_id: String::new(),
            listener_id: None,
            manager: background_session_manager(),
        }
    }

    pub fn unsubscribe(mut self) {
        if let Some(listener_id) = self.listener_id.take() {
            self.manager.unsubscribe(&self.session_id, listener_id);
        }
    }
}

impl Drop for BackgroundSessionSubscription {
    fn drop(&mut self) {
        if let Some(listener_id) = self.listener_id.take() {
            self.manager.unsubscribe(&self.session_id, listener_id);
        }
    }
}

struct BackgroundSession {
    session_id: String,
    command: String,
    shell: Option<String>,
    cwd: PathBuf,
    started_at: Instant,
    timeout_seconds: u64,
    child: Option<Child>,
    output_path: PathBuf,
    status: BackgroundStatus,
    output: String,
    exit_code: Option<i32>,
    listeners: BTreeMap<u64, BackgroundSessionListener>,
}

impl BackgroundSession {
    fn running_snapshot(&self, elapsed: Duration) -> Value {
        let mut payload = json!({
            "status": "running",
            "session_id": self.session_id,
            "command": self.command,
            "elapsed_seconds": (elapsed.as_millis() as f64) / 1000.0,
            "cwd": display_path(&self.cwd),
        });
        if let Some(shell) = &self.shell {
            payload["shell"] = Value::String(shell.clone());
        }
        payload
    }

    fn snapshot(&self) -> Value {
        let mut payload = json!({
            "status": self.status.as_str(),
            "session_id": self.session_id,
            "command": self.command,
            "cwd": display_path(&self.cwd),
            "exit_code": self.exit_code,
            "output": self.output,
        });
        if let Some(shell) = &self.shell {
            payload["shell"] = Value::String(shell.clone());
        }
        payload
    }

    fn finalize_completed(&mut self, exit_code: i32) {
        self.exit_code = Some(exit_code);
        self.status = if exit_code == 0 {
            BackgroundStatus::Completed
        } else {
            BackgroundStatus::Failed
        };
        self.output = read_captured_output(&self.output_path, OUTPUT_LIMIT);
        remove_captured_output(&self.output_path);
        self.child = None;
    }

    fn finalize_timeout(&mut self) {
        if let Some(child) = self.child.as_mut() {
            kill_process_tree(child);
            self.exit_code = Some(
                child
                    .try_wait()
                    .ok()
                    .flatten()
                    .and_then(|s| s.code())
                    .unwrap_or(-9),
            );
        } else {
            self.exit_code = Some(-9);
        }
        self.status = BackgroundStatus::Timeout;
        self.output = read_captured_output(&self.output_path, OUTPUT_LIMIT);
        if self.output.is_empty() {
            self.output = "Command timed out in background session".to_string();
        }
        remove_captured_output(&self.output_path);
        self.child = None;
    }

    fn take_listeners(&mut self) -> Vec<BackgroundSessionListener> {
        std::mem::take(&mut self.listeners).into_values().collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BackgroundStatus {
    Running,
    Completed,
    Failed,
    Timeout,
}

impl BackgroundStatus {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Timeout)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
        }
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn notify_background_listeners(listeners: Vec<BackgroundSessionListener>, payload: &Value) {
    for listener in listeners {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            listener(payload);
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn notify_background_listeners_continues_after_listener_panic_like_python() {
        let delivered = Arc::new(AtomicUsize::new(0));
        let delivered_listener = Arc::clone(&delivered);
        let listeners: Vec<BackgroundSessionListener> = vec![
            Arc::new(|_| panic!("boom")),
            Arc::new(move |_| {
                delivered_listener.fetch_add(1, Ordering::Relaxed);
            }),
        ];

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            notify_background_listeners(listeners, &json!({"status": "completed"}));
        }));

        assert!(result.is_ok());
        assert_eq!(delivered.load(Ordering::Relaxed), 1);
    }
}
