use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::runtime::processes::{kill_process_tree, read_captured_output, remove_captured_output};

use super::listeners::BackgroundSessionListener;
use super::options::BackgroundSessionAdoptOptions;

const OUTPUT_LIMIT: usize = 50_000;

pub(in crate::runtime::background_sessions) struct BackgroundSession {
    session_id: String,
    command: String,
    shell: Option<String>,
    cwd: PathBuf,
    started_at: Instant,
    timeout_seconds: u64,
    child: Option<std::process::Child>,
    output_path: PathBuf,
    status: BackgroundStatus,
    output: String,
    exit_code: Option<i32>,
    listeners: BTreeMap<u64, BackgroundSessionListener>,
}

impl BackgroundSession {
    pub(in crate::runtime::background_sessions) fn from_adopt_options(
        session_id: String,
        options: BackgroundSessionAdoptOptions,
    ) -> Self {
        Self {
            session_id,
            command: options.command,
            shell: options.shell,
            cwd: options.cwd,
            started_at: options.started_at.unwrap_or_else(Instant::now),
            timeout_seconds: options.timeout_seconds.max(1),
            child: Some(options.child),
            output_path: options.output_path,
            status: BackgroundStatus::Running,
            output: String::new(),
            exit_code: None,
            listeners: BTreeMap::new(),
        }
    }

    pub(in crate::runtime::background_sessions) fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }

    pub(in crate::runtime::background_sessions) fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    pub(in crate::runtime::background_sessions) fn timed_out(&self, elapsed: Duration) -> bool {
        elapsed > Duration::from_secs(self.timeout_seconds)
    }

    pub(in crate::runtime::background_sessions) fn try_wait(
        &mut self,
    ) -> std::io::Result<Option<i32>> {
        let Some(child) = self.child.as_mut() else {
            self.finalize_completed(0);
            return Ok(Some(0));
        };
        child
            .try_wait()
            .map(|status| status.map(|exit_status| exit_status.code().unwrap_or(-1)))
    }

    pub(in crate::runtime::background_sessions) fn add_listener(
        &mut self,
        listener_id: u64,
        listener: BackgroundSessionListener,
    ) {
        self.listeners.insert(listener_id, listener);
    }

    pub(in crate::runtime::background_sessions) fn remove_listener(&mut self, listener_id: u64) {
        self.listeners.remove(&listener_id);
    }

    pub(in crate::runtime::background_sessions) fn running_snapshot(
        &self,
        elapsed: Duration,
    ) -> Value {
        json!({
            "status": "running",
            "session_id": self.session_id,
            "command": self.command,
            "elapsed_seconds": (elapsed.as_millis() as f64) / 1000.0,
            "cwd": display_path(&self.cwd),
            "shell": self.shell,
        })
    }

    pub(in crate::runtime::background_sessions) fn snapshot(&self) -> Value {
        json!({
            "status": self.status.as_str(),
            "session_id": self.session_id,
            "command": self.command,
            "cwd": display_path(&self.cwd),
            "exit_code": self.exit_code,
            "output": self.output,
            "shell": self.shell,
        })
    }

    pub(in crate::runtime::background_sessions) fn finalize_completed(&mut self, exit_code: i32) {
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

    pub(in crate::runtime::background_sessions) fn finalize_failed_with_output(
        &mut self,
        exit_code: i32,
        output: String,
    ) {
        self.status = BackgroundStatus::Failed;
        self.exit_code = Some(exit_code);
        self.output = output;
        self.child = None;
    }

    pub(in crate::runtime::background_sessions) fn finalize_timeout(&mut self) {
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

    pub(in crate::runtime::background_sessions) fn take_listeners(
        &mut self,
    ) -> Vec<BackgroundSessionListener> {
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
