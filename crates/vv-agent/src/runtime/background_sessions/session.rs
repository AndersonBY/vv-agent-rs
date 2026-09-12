use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::runtime::processes::{
    kill_process_tree, observed_exit_code, process_tree_is_running, remove_captured_output,
};
use crate::types::ToolArtifactRef;
use crate::workspace::{
    artifact_write_error_code, bounded_captured_text_preview, persist_captured_text_artifact,
    snapshot_captured_text, BoundedTextPreview, WorkspaceBackend,
};

use super::listeners::BackgroundSessionListener;
use super::options::BackgroundSessionAdoptOptions;

pub(super) struct BackgroundSession {
    session_id: String,
    command: String,
    shell: Option<String>,
    started_at: Instant,
    timeout_seconds: Option<u64>,
    child: Option<crate::runtime::processes::ManagedChild>,
    output_path: PathBuf,
    owner_task_id: String,
    owner_workspace: PathBuf,
    status: BackgroundStatus,
    stop_reason: Option<BackgroundStatus>,
    preview: Option<BoundedTextPreview>,
    artifact: Option<ToolArtifactRef>,
    artifact_error: Option<String>,
    artifact_error_code: Option<String>,
    output_error: Option<String>,
    observation_error: Option<String>,
    artifact_backend: Option<Arc<dyn WorkspaceBackend>>,
    artifact_task_id: String,
    artifact_tool_call_id: String,
    exit_code: Option<i32>,
    listeners: BTreeMap<u64, BackgroundSessionListener>,
}

impl BackgroundSession {
    pub(super) fn from_adopt_options(
        session_id: String,
        options: BackgroundSessionAdoptOptions,
    ) -> Self {
        Self {
            session_id,
            command: options.command,
            shell: options.shell,
            started_at: options.started_at.unwrap_or_else(Instant::now),
            timeout_seconds: options.timeout_seconds,
            child: Some(options.child),
            output_path: options.output_path,
            owner_task_id: options.owner_task_id,
            owner_workspace: options.owner_workspace,
            status: BackgroundStatus::Running,
            stop_reason: None,
            preview: None,
            artifact: None,
            artifact_error: None,
            artifact_error_code: None,
            output_error: None,
            observation_error: None,
            artifact_backend: options.artifact_backend,
            artifact_task_id: options.artifact_task_id,
            artifact_tool_call_id: options.artifact_tool_call_id,
            exit_code: None,
            listeners: BTreeMap::new(),
        }
    }

    pub(super) fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            BackgroundStatus::Completed
                | BackgroundStatus::Failed
                | BackgroundStatus::Timeout
                | BackgroundStatus::Stopped
        )
    }

    pub(super) fn remaining_yield(&self, duration: Duration) -> Duration {
        duration.saturating_sub(self.started_at.elapsed())
    }

    pub(super) fn owned_by(&self, task_id: &str, workspace: &Path) -> bool {
        self.owner_task_id == task_id
            && workspace
                .canonicalize()
                .is_ok_and(|root| root == self.owner_workspace)
    }

    pub(super) fn set_artifact_context(
        &mut self,
        backend: Arc<dyn WorkspaceBackend>,
        task_id: &str,
        call_id: &str,
    ) {
        if self.artifact_backend.is_none() {
            self.artifact_backend = Some(backend);
            self.artifact_task_id = task_id.to_string();
            self.artifact_tool_call_id = call_id.to_string();
        }
    }

    pub(super) fn advance(&mut self) -> Vec<BackgroundSessionListener> {
        if self.is_terminal() {
            self.ensure_terminal_output();
            return Vec::new();
        }
        let observation = self
            .child
            .as_mut()
            .ok_or_else(|| std::io::Error::other("process observation unavailable"))
            .and_then(|child| {
                let exit_code = child.try_wait()?.and_then(observed_exit_code);
                let running = if exit_code.is_some() {
                    process_tree_is_running(child)?
                } else {
                    true
                };
                Ok((exit_code, running))
            });
        match observation {
            Ok((Some(exit_code), false)) => return self.finish(exit_code),
            Ok(_) => {
                self.status = if self.stop_reason.is_some() {
                    BackgroundStatus::Stopping
                } else {
                    BackgroundStatus::Running
                };
                self.observation_error = None;
            }
            Err(error) => {
                self.status = BackgroundStatus::Unknown;
                self.observation_error = Some(error.to_string());
            }
        }
        if self
            .timeout_seconds
            .is_some_and(|timeout| self.started_at.elapsed() >= Duration::from_secs(timeout))
        {
            return self.request_stop(true);
        }
        Vec::new()
    }

    pub(super) fn request_stop(&mut self, timeout: bool) -> Vec<BackgroundSessionListener> {
        if self.is_terminal() {
            return Vec::new();
        }
        self.stop_reason.get_or_insert(if timeout {
            BackgroundStatus::Timeout
        } else {
            BackgroundStatus::Stopped
        });
        self.status = BackgroundStatus::Stopping;
        let Some(child) = self.child.as_mut() else {
            self.status = BackgroundStatus::Unknown;
            self.observation_error = Some("process observation unavailable".to_string());
            return Vec::new();
        };
        if kill_process_tree(child) {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if let Some(exit_code) = observed_exit_code(status) {
                        return self.finish(exit_code);
                    }
                }
                Err(error) => {
                    self.status = BackgroundStatus::Unknown;
                    self.observation_error = Some(error.to_string());
                }
                Ok(None) => {}
            }
        }
        Vec::new()
    }

    fn finish(&mut self, exit_code: i32) -> Vec<BackgroundSessionListener> {
        self.exit_code = Some(exit_code);
        self.status = self.stop_reason.unwrap_or(if exit_code == 0 {
            BackgroundStatus::Completed
        } else {
            BackgroundStatus::Failed
        });
        self.ensure_terminal_output();
        self.child = None;
        std::mem::take(&mut self.listeners).into_values().collect()
    }

    fn ensure_terminal_output(&mut self) {
        if !self.is_terminal() {
            return;
        }
        if self.preview.is_none() {
            match bounded_captured_text_preview(&self.output_path) {
                Ok(preview) => {
                    if !preview.truncated {
                        remove_captured_output(&self.output_path);
                    }
                    self.preview = Some(preview);
                    self.output_error = None;
                }
                Err(error) => {
                    self.output_error = Some(error.to_string());
                    return;
                }
            }
        }
        if self
            .preview
            .as_ref()
            .is_some_and(|preview| !preview.truncated)
        {
            return;
        }
        if self.artifact.is_some() {
            return;
        }
        let Some(backend) = self.artifact_backend.clone() else {
            return;
        };
        match persist_captured_text_artifact(
            backend,
            &self.artifact_task_id,
            &self.artifact_tool_call_id,
            &self.output_path,
        ) {
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

    pub(super) fn add_listener(&mut self, id: u64, listener: BackgroundSessionListener) {
        self.listeners.insert(id, listener);
    }

    pub(super) fn remove_listener(&mut self, id: u64) {
        self.listeners.remove(&id);
    }

    pub(super) fn snapshot(&self) -> Value {
        let mut payload = json!({"status": self.status.as_str(), "session_id": self.session_id, "command": self.command});
        if let Some(shell) = &self.shell {
            payload["shell"] = json!(shell);
        }
        if let Some(exit_code) = self.exit_code {
            payload["exit_code"] = json!(exit_code);
        } else {
            payload["elapsed_seconds"] =
                json!((self.started_at.elapsed().as_secs_f64() * 100.0).round() / 100.0);
        }
        if let Some(error) = &self.observation_error {
            payload["observation_error"] = json!(error);
        }
        if let Some(error) = &self.output_error {
            payload["output_error"] = json!(error);
        }
        if let Some(error) = &self.artifact_error {
            payload["artifact_error"] = json!(error);
            payload["artifact_error_code"] = json!(self.artifact_error_code);
        }
        if let Some(preview) = &self.preview {
            add_preview(&mut payload, preview);
        }
        if let Some(artifact) = &self.artifact {
            payload["artifact"] = json!(artifact);
        }
        payload
    }

    pub(super) fn live_snapshot(&self) -> BackgroundOutputSnapshot {
        BackgroundOutputSnapshot {
            payload: self.snapshot(),
            output_path: (!self.is_terminal()).then(|| self.output_path.clone()),
            artifact_backend: self.artifact_backend.clone(),
            artifact_task_id: self.artifact_task_id.clone(),
            artifact_tool_call_id: self.artifact_tool_call_id.clone(),
        }
    }
}

/// State is captured under the session lock; live storage I/O runs outside it.
/// A slow artifact backend must not prevent the watchdog from stopping a child.
pub(super) struct BackgroundOutputSnapshot {
    payload: Value,
    output_path: Option<PathBuf>,
    artifact_backend: Option<Arc<dyn WorkspaceBackend>>,
    artifact_task_id: String,
    artifact_tool_call_id: String,
}

impl BackgroundOutputSnapshot {
    pub(super) fn render(self) -> Value {
        let mut payload = self.payload;
        let Some(output_path) = self.output_path else {
            return payload;
        };
        match snapshot_captured_text(&output_path).and_then(|snapshot| {
            let preview = bounded_captured_text_preview(&snapshot.path)?;
            add_preview(&mut payload, &preview);
            if preview.truncated {
                if let Some(backend) = &self.artifact_backend {
                    match persist_captured_text_artifact(
                        backend.clone(),
                        &self.artifact_task_id,
                        &self.artifact_tool_call_id,
                        &snapshot.path,
                    ) {
                        Ok(artifact) => payload["artifact"] = json!(artifact),
                        Err(error) => {
                            payload["artifact_error"] = json!(error.to_string());
                            payload["artifact_error_code"] =
                                json!(artifact_write_error_code(&error));
                        }
                    }
                }
            }
            Ok(())
        }) {
            Ok(()) => {}
            Err(error) => payload["output_error"] = json!(error.to_string()),
        }
        payload
    }
}

fn add_preview(payload: &mut Value, preview: &BoundedTextPreview) {
    payload["output"] = json!(preview.content);
    payload["output_truncated"] = json!(preview.truncated);
    payload["output_json_bytes"] = json!(preview.json_bytes);
    if preview.truncated {
        payload["output_original_bytes"] = json!(preview.original_bytes);
        payload["output_visible_bytes"] = json!(preview.visible_bytes);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BackgroundStatus {
    Running,
    Stopping,
    Unknown,
    Completed,
    Failed,
    Timeout,
    Stopped,
}

impl BackgroundStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Unknown => "unknown",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::Stopped => "stopped",
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::runtime::background_sessions::background_session_manager;
    use crate::runtime::processes::start_captured_process;
    use crate::tools::{build_default_registry, ToolContext};
    use crate::types::{ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct LocalSession {
        root: tempfile::TempDir,
        id: String,
        session: Arc<Mutex<BackgroundSession>>,
    }

    impl LocalSession {
        fn new(group: bool) -> Self {
            let root = tempfile::tempdir().unwrap();
            let (child, output_path) = if group {
                let captured = start_captured_process(
                    &["sleep".to_string(), "10".to_string()],
                    root.path(),
                    None,
                )
                .unwrap();
                (captured.child, captured.output_path)
            } else {
                let child = Command::new("sleep")
                    .arg("10")
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap();
                let output = root.path().join("capture.log");
                std::fs::write(&output, "known output").unwrap();
                (child.into(), output)
            };
            Self::insert(root, child, output_path)
        }

        fn insert(
            root: tempfile::TempDir,
            child: crate::runtime::processes::ManagedChild,
            output_path: PathBuf,
        ) -> Self {
            let id = format!("unit_{}", uuid::Uuid::new_v4().simple());
            let options = BackgroundSessionAdoptOptions::new(
                "sleep 10",
                root.path(),
                None,
                child,
                output_path,
            )
            .with_owner("owner", root.path());
            let session = Arc::new(Mutex::new(BackgroundSession::from_adopt_options(
                id.clone(),
                options,
            )));
            // No watchdog: the test can prove the requested operation itself has zero effects.
            background_session_manager()
                .sessions
                .lock()
                .unwrap()
                .insert(id.clone(), session.clone());
            Self { root, id, session }
        }

        fn call(&self, name: &str, task: &str, workspace: &Path) -> ToolExecutionResult {
            let mut context = ToolContext::new(workspace);
            context.task_id = task.to_string();
            context.tool_call_id = "management".to_string();
            build_default_registry()
                .execute(
                    &ToolCall::new(
                        "management",
                        name,
                        BTreeMap::from([("session_id".to_string(), json!(self.id))]),
                    ),
                    &mut context,
                )
                .unwrap()
        }
    }

    impl Drop for LocalSession {
        fn drop(&mut self) {
            let mut session = self.session.lock().unwrap();
            if let Some(mut child) = session.child.take() {
                if !kill_process_tree(&mut child) {
                    // The unconfirmed-stop case intentionally has no dedicated process group.
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            remove_captured_output(&session.output_path);
            background_session_manager()
                .sessions
                .lock()
                .unwrap()
                .remove(&self.id);
        }
    }

    struct DelayedArtifactBackend {
        inner: crate::workspace::LocalWorkspaceBackend,
        entered: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        first: std::sync::atomic::AtomicBool,
    }

    impl WorkspaceBackend for DelayedArtifactBackend {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>> {
            self.inner.list_files(base, glob)
        }
        fn read_text(&self, path: &str) -> std::io::Result<String> {
            self.inner.read_text(path)
        }
        fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
            self.inner.read_bytes(path)
        }
        fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
            self.inner.write_text(path, content, append)
        }
        fn write_text_exclusive(&self, path: &str, content: &str) -> std::io::Result<usize> {
            self.inner.write_text_exclusive(path, content)
        }
        fn write_text_chunks_exclusive(
            &self,
            path: &str,
            chunks: &mut dyn Iterator<Item = std::io::Result<String>>,
        ) -> std::io::Result<usize> {
            if self.first.swap(false, Ordering::SeqCst) {
                self.entered.send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
            }
            self.inner.write_text_chunks_exclusive(path, chunks)
        }
        fn file_info(&self, path: &str) -> std::io::Result<Option<crate::workspace::FileInfo>> {
            self.inner.file_info(path)
        }
        fn exists(&self, path: &str) -> bool {
            self.inner.exists(path)
        }
        fn is_file(&self, path: &str) -> bool {
            self.inner.is_file(path)
        }
        fn mkdir(&self, path: &str) -> std::io::Result<()> {
            self.inner.mkdir(path)
        }
    }

    #[test]
    fn slow_live_artifact_does_not_block_execution_deadline() {
        let root = tempfile::tempdir().unwrap();
        let captured = start_captured_process(
            &[
                "sh".into(),
                "-c".into(),
                "printf '%13001s' ''; sleep 10".into(),
            ],
            root.path(),
            None,
        )
        .unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let backend = Arc::new(DelayedArtifactBackend {
            inner: crate::workspace::LocalWorkspaceBackend::new(root.path()),
            entered: entered_tx,
            release: Mutex::new(release_rx),
            first: std::sync::atomic::AtomicBool::new(true),
        });
        let capture_path = captured.output_path.clone();
        let options = BackgroundSessionAdoptOptions::new(
            "large-output",
            root.path(),
            Some(1),
            captured.child,
            captured.output_path,
        )
        .with_started_at(captured.started_at)
        .with_owner("owner", root.path())
        .with_artifact_context(backend, "owner", "call");
        let manager = background_session_manager();
        let id = manager.adopt_running_process_with_options(options);
        let session = manager.get(&id).unwrap();
        let ready_deadline = Instant::now() + Duration::from_secs(2);
        while std::fs::metadata(&capture_path).unwrap().len() < 13001 {
            assert!(Instant::now() < ready_deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let query_id = id.clone();
        let query = std::thread::spawn(move || background_session_manager().check(&query_id));
        let entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut timed_out = false;
        while Instant::now() < deadline {
            if session
                .try_lock()
                .is_ok_and(|state| state.status == BackgroundStatus::Timeout)
            {
                timed_out = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Always release our own I/O before asserting, even on the red run.
        release_tx.send(()).unwrap();
        query.join().unwrap();
        session.lock().unwrap().request_stop(false);
        assert!(entered, "live query never entered artifact persistence");
        assert!(timed_out, "live artifact I/O blocked the process deadline");
    }

    #[test]
    fn foreign_queries_and_stops_do_not_poll_expired_deadline_read_output_or_notify() {
        let local = LocalSession::new(true);
        let foreign_workspace = tempfile::tempdir().unwrap();
        let notifications = Arc::new(AtomicUsize::new(0));
        {
            let mut session = local.session.lock().unwrap();
            session.started_at = Instant::now() - Duration::from_secs(2);
            session.timeout_seconds = Some(1);
            let observed = notifications.clone();
            session.add_listener(
                1,
                Arc::new(move |_| {
                    observed.fetch_add(1, Ordering::SeqCst);
                }),
            );
            // Any output read would fail; ownership rejection must happen first.
            remove_captured_output(&session.output_path);
        }
        for (task, workspace) in [
            ("foreign", local.root.path()),
            ("owner", foreign_workspace.path()),
        ] {
            for name in ["check_background_command", "stop_background_command"] {
                let result = local.call(name, task, workspace);
                assert_eq!(result.status, ToolResultStatus::Error);
                assert_eq!(
                    result.error_code.as_deref(),
                    Some("background_session_forbidden")
                );
            }
        }
        let mut session = local.session.lock().unwrap();
        assert!(session
            .child
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none());
        assert!(session.status == BackgroundStatus::Running);
        assert!(session.stop_reason.is_none());
        assert!(session.artifact_backend.is_none());
        assert!(session.output_error.is_none());
        assert_eq!(session.listeners.len(), 1);
        assert_eq!(notifications.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn real_unconfirmed_tree_stop_returns_success_without_an_exit_code() {
        let local = LocalSession::new(false);
        let result = local.call("stop_background_command", "owner", local.root.path());
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(result.directive, ToolDirective::Continue);
        assert_eq!(result.metadata["status"], "stopping");
        assert!(!result.metadata.contains_key("exit_code"));
        assert!(
            serde_json::from_str::<Value>(&result.content).unwrap()["message"]
                .as_str()
                .unwrap()
                .contains("unconfirmed")
        );
        assert!(serde_json::from_str::<Value>(&result.content)
            .unwrap()
            .get("exit_code")
            .is_none());
        assert!(local
            .session
            .lock()
            .unwrap()
            .child
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none());
    }

    #[test]
    fn unavailable_process_observation_remains_unknown_without_inventing_exit() {
        let local = LocalSession::new(true);
        let child = local.session.lock().unwrap().child.take();
        let result = local.call("check_background_command", "owner", local.root.path());
        local.session.lock().unwrap().child = child;
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(result.metadata["status"], "unknown");
        assert!(!result.metadata.contains_key("exit_code"));
        assert!(
            serde_json::from_str::<Value>(&result.content).unwrap()["message"]
                .as_str()
                .unwrap()
                .contains("unconfirmed")
        );
        assert!(serde_json::from_str::<Value>(&result.content)
            .unwrap()
            .get("exit_code")
            .is_none());
    }

    #[test]
    fn terminal_capture_read_failure_retries_without_duplicate_notification() {
        for output_size in [17, 13001] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("source.txt"), "x".repeat(output_size)).unwrap();
            let mut captured = start_captured_process(
                &["cat".to_string(), "source.txt".to_string()],
                root.path(),
                None,
            )
            .unwrap();
            captured.child.wait().unwrap();
            let local = LocalSession::insert(root, captured.child, captured.output_path);
            let notifications = Arc::new(AtomicUsize::new(0));
            let path = {
                let mut session = local.session.lock().unwrap();
                let observed = notifications.clone();
                session.add_listener(
                    1,
                    Arc::new(move |_| {
                        observed.fetch_add(1, Ordering::SeqCst);
                    }),
                );
                session.output_path.clone()
            };
            let saved = local.root.path().join("saved-capture");
            std::fs::rename(&path, &saved).unwrap();
            let failed = local.call("check_background_command", "owner", local.root.path());
            assert_eq!(failed.status, ToolResultStatus::Error);
            assert_eq!(failed.metadata["status"], "completed");
            assert_eq!(notifications.load(Ordering::SeqCst), 1);
            assert_eq!(
                std::fs::read_to_string(&saved).unwrap(),
                "x".repeat(output_size)
            );
            std::fs::rename(&saved, &path).unwrap();
            let recovered = local.call("check_background_command", "owner", local.root.path());
            assert_eq!(recovered.status, ToolResultStatus::Success);
            assert_eq!(recovered.metadata["exit_code"], 0);
            assert!(local.session.lock().unwrap().output_error.is_none());
            assert_eq!(notifications.load(Ordering::SeqCst), 1);
            if output_size > 12000 {
                let backend = local
                    .session
                    .lock()
                    .unwrap()
                    .artifact_backend
                    .clone()
                    .unwrap();
                assert_eq!(
                    backend
                        .read_text(&recovered.artifact.as_ref().unwrap().path)
                        .unwrap(),
                    "x".repeat(output_size)
                );
            } else {
                assert_eq!(recovered.content, "x".repeat(output_size));
            }
            // Once the original capture is released, a repeated receipt must
            // not delete a different file subsequently created at that path.
            std::fs::write(&path, "REPLACEMENT_CAPTURE").unwrap();
            let repeated = local.call("check_background_command", "owner", local.root.path());
            assert_eq!(repeated.content, recovered.content);
            assert_eq!(repeated.artifact, recovered.artifact);
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "REPLACEMENT_CAPTURE"
            );
            std::fs::remove_file(&path).unwrap();
        }
    }

    #[test]
    fn live_capture_read_failure_is_retryable() {
        let local = LocalSession::new(true);
        let path = local.session.lock().unwrap().output_path.clone();
        std::fs::write(&path, "LIVE_RECOVERED").unwrap();
        let saved = local.root.path().join("saved-capture");
        std::fs::rename(&path, &saved).unwrap();
        let failed = local.call("check_background_command", "owner", local.root.path());
        std::fs::rename(&saved, &path).unwrap();
        assert_eq!(failed.status, ToolResultStatus::Error);
        assert_eq!(failed.metadata["status"], "running");
        let recovered = local.call("check_background_command", "owner", local.root.path());
        assert_eq!(recovered.status, ToolResultStatus::Success);
        assert_eq!(recovered.metadata["status"], "running");
        assert!(recovered.content.contains("LIVE_RECOVERED"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn lost_supervisor_does_not_claim_its_signal_as_command_exit() {
        let root = tempfile::tempdir().unwrap();
        let captured = start_captured_process(&[
            "sh".to_string(), "-c".to_string(),
            "echo $$ > root-pid; printf KEEP_CAPTURE; while test ! -e finish; do sleep 0.01; done".to_string(),
        ], root.path(), None).unwrap();
        let local = LocalSession::insert(root, captured.child, captured.output_path);
        struct Release(PathBuf);
        impl Drop for Release {
            fn drop(&mut self) {
                let _ = std::fs::write(&self.0, "");
            }
        }
        let release = Release(local.root.path().join("finish"));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if std::fs::read_to_string(&local.session.lock().unwrap().output_path)
                .unwrap()
                .contains("KEEP_CAPTURE")
            {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let root_pid = std::fs::read_to_string(local.root.path().join("root-pid"))
            .unwrap()
            .trim()
            .to_string();
        {
            let mut session = local.session.lock().unwrap();
            let child = session.child.as_mut().unwrap();
            assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGKILL) }, 0);
            child.wait().unwrap();
        }
        let result = local.call("check_background_command", "owner", local.root.path());
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(result.metadata["status"], "unknown");
        assert!(!result.metadata.contains_key("exit_code"));
        assert!(result.content.contains("unconfirmed"));
        assert!(local.session.lock().unwrap().output_path.exists());
        assert!(!local.session.lock().unwrap().is_terminal());
        drop(release);
        loop {
            let running =
                std::fs::read_to_string(format!("/proc/{root_pid}/stat")).is_ok_and(|stat| {
                    !matches!(
                        stat.rsplit_once(')').unwrap().1.split_whitespace().next(),
                        Some("Z" | "X")
                    )
                });
            if !running {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
