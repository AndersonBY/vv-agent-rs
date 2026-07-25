use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::runtime::processes::{
    kill_process_tree, read_captured_output_all, remove_captured_output,
};
use crate::types::ToolArtifactRef;
use crate::workspace::{
    artifact_write_error_code, bounded_text_preview, persist_text_artifact, WorkspaceBackend,
};

use super::listeners::BackgroundSessionListener;
use super::options::BackgroundSessionAdoptOptions;

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
    artifact: Option<ToolArtifactRef>,
    artifact_error: Option<String>,
    artifact_error_code: Option<String>,
    artifact_backend: Option<std::sync::Arc<dyn WorkspaceBackend>>,
    artifact_task_id: String,
    artifact_tool_call_id: String,
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
            artifact: None,
            artifact_error: None,
            artifact_error_code: None,
            artifact_backend: options.artifact_backend,
            artifact_task_id: options.artifact_task_id,
            artifact_tool_call_id: options.artifact_tool_call_id,
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
        let preview = bounded_text_preview(&self.output);
        json!({
            "status": self.status.as_str(),
            "session_id": self.session_id,
            "command": self.command,
            "cwd": display_path(&self.cwd),
            "exit_code": self.exit_code,
            "output": preview.content,
            "shell": self.shell,
            "output_truncated": preview.truncated,
            "output_original_bytes": preview.truncated.then_some(preview.original_bytes),
            "output_visible_bytes": preview.truncated.then_some(preview.visible_bytes),
            "artifact": self.artifact,
            "artifact_error": self.artifact_error,
            "artifact_error_code": self.artifact_error_code,
        })
    }

    pub(in crate::runtime::background_sessions) fn finalize_completed(&mut self, exit_code: i32) {
        self.exit_code = Some(exit_code);
        self.status = if exit_code == 0 {
            BackgroundStatus::Completed
        } else {
            BackgroundStatus::Failed
        };
        self.capture_terminal_output(String::new());
        self.child = None;
    }

    pub(in crate::runtime::background_sessions) fn finalize_failed_with_output(
        &mut self,
        exit_code: i32,
        output: String,
    ) {
        self.status = BackgroundStatus::Failed;
        self.exit_code = Some(exit_code);
        self.capture_terminal_output(output);
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
        self.capture_terminal_output(String::new());
        if self.output.is_empty() {
            self.output = "Command timed out in background session".to_string();
        }
        self.child = None;
    }

    pub(in crate::runtime::background_sessions) fn ensure_artifact(
        &mut self,
        fallback_backend: std::sync::Arc<dyn WorkspaceBackend>,
        fallback_task_id: &str,
        fallback_tool_call_id: &str,
    ) {
        if !self.is_terminal() || !bounded_text_preview(&self.output).truncated {
            return;
        }
        if self.artifact.is_some() {
            return;
        }
        let backend = self.artifact_backend.clone().unwrap_or(fallback_backend);
        let task_id = if self.artifact_task_id.trim().is_empty() {
            fallback_task_id
        } else {
            &self.artifact_task_id
        };
        let tool_call_id = if self.artifact_tool_call_id.trim().is_empty() {
            fallback_tool_call_id
        } else {
            &self.artifact_tool_call_id
        };
        match persist_text_artifact(backend, task_id, tool_call_id, &self.output) {
            Ok(artifact) => {
                self.artifact = Some(artifact);
                self.artifact_error = None;
                self.artifact_error_code = None;
                remove_captured_output(&self.output_path);
            }
            Err(error) => {
                self.artifact_error_code = Some(artifact_write_error_code(&error).to_string());
                self.artifact_error = Some(error.to_string());
            }
        }
    }

    fn capture_terminal_output(&mut self, fallback: String) {
        self.output = read_captured_output_all(&self.output_path).unwrap_or(fallback);
        if !bounded_text_preview(&self.output).truncated {
            remove_captured_output(&self.output_path);
        }
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
