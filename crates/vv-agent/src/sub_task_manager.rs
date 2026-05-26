use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::sub_agent_sessions::{
    continue_sub_agent_session, get_sub_agent_session, SubAgentSession, SubAgentSessionListener,
};
use crate::types::{AgentStatus, SubTaskOutcome};
use crate::workspace::WorkspaceBackend;

static SUB_TASK_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Default)]
pub struct SubTaskManager {
    tasks: Arc<Mutex<BTreeMap<String, ManagedSubTask>>>,
}

impl std::fmt::Debug for SubTaskManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubTaskManager")
            .finish_non_exhaustive()
    }
}

impl SubTaskManager {
    pub fn next_task_identity(parent_task_id: &str, agent_name: &str) -> (String, String) {
        let parent = parent_task_id.trim();
        let parent = if parent.is_empty() { "task" } else { parent };
        let suffix = SUB_TASK_COUNTER.fetch_add(1, Ordering::Relaxed);
        let task_id = format!("{parent}_sub_{agent_name}_{suffix:08x}");
        (task_id.clone(), task_id)
    }

    pub fn submit(
        &self,
        task_id: impl Into<String>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        task_title: impl Into<String>,
        runner: impl FnOnce() -> SubTaskOutcome + Send + 'static,
    ) {
        self.submit_with_workspace(task_id, session_id, agent_name, task_title, None, runner);
    }

    pub fn submit_with_workspace(
        &self,
        task_id: impl Into<String>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        task_title: impl Into<String>,
        workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
        runner: impl FnOnce() -> SubTaskOutcome + Send + 'static,
    ) {
        let task_id = task_id.into();
        let session_id = session_id.into();
        let agent_name = agent_name.into();
        let task_title = task_title.into();
        {
            let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
            tasks.insert(
                task_id.clone(),
                ManagedSubTask {
                    task_id: task_id.clone(),
                    session_id: session_id.clone(),
                    agent_name: agent_name.clone(),
                    task_title,
                    workspace_backend,
                    session: None,
                    outcome: None,
                    current_cycle_index: None,
                    recent_activity: None,
                    latest_cycle: None,
                    latest_tool_call: None,
                    handle: None,
                    updated_at: now_millis(),
                    manager_listener_attached: false,
                },
            );
        }

        let tasks = self.tasks.clone();
        let task_id_for_thread = task_id.clone();
        let handle = thread::spawn(move || {
            let outcome = runner();
            let mut tasks = tasks.lock().expect("sub-task manager poisoned");
            if let Some(record) = tasks.get_mut(&task_id_for_thread) {
                record.update_from_outcome(&outcome);
                record.outcome = Some(outcome);
                record.updated_at = now_millis();
                record.handle = None;
            }
        });

        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        if let Some(record) = tasks.get_mut(&task_id) {
            record.handle = Some(handle);
            record.updated_at = now_millis();
        }
    }

    pub fn record_outcome(&self, task_id: &str, outcome: SubTaskOutcome) {
        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        let task_id = task_id.to_string();
        match tasks.get_mut(&task_id) {
            Some(record) => {
                record.session_id = outcome
                    .session_id
                    .clone()
                    .unwrap_or_else(|| record.session_id.clone());
                record.agent_name = outcome.agent_name.clone();
                record.update_from_outcome(&outcome);
                record.outcome = Some(outcome);
                record.updated_at = now_millis();
            }
            None => {
                let mut record = ManagedSubTask {
                    session_id: outcome.session_id.clone().unwrap_or_default(),
                    agent_name: outcome.agent_name.clone(),
                    task_title: String::new(),
                    workspace_backend: None,
                    session: None,
                    outcome: None,
                    current_cycle_index: None,
                    recent_activity: None,
                    latest_cycle: None,
                    latest_tool_call: None,
                    task_id: task_id.clone(),
                    handle: None,
                    updated_at: now_millis(),
                    manager_listener_attached: false,
                };
                record.update_from_outcome(&outcome);
                record.outcome = Some(outcome);
                tasks.insert(task_id.clone(), record);
            }
        }
    }

    pub fn attach_session(
        &self,
        task_id: impl Into<String>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        task_title: impl Into<String>,
        workspace_backend: Arc<dyn WorkspaceBackend>,
        session: Arc<dyn SubAgentSession>,
    ) {
        let task_id = task_id.into();
        let session_id = session_id.into();
        let agent_name = agent_name.into();
        let task_title = task_title.into();
        let should_attach_listener = {
            let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
            match tasks.get_mut(&task_id) {
                Some(record) => {
                    record.session_id = session_id;
                    record.agent_name = agent_name;
                    if !task_title.is_empty() {
                        record.task_title = task_title;
                    }
                    record.workspace_backend = Some(workspace_backend);
                    record.session = Some(session.clone());
                    record.updated_at = now_millis();
                    let should_attach_listener = !record.manager_listener_attached;
                    record.manager_listener_attached = true;
                    should_attach_listener
                }
                None => {
                    tasks.insert(
                        task_id.clone(),
                        ManagedSubTask {
                            task_id: task_id.clone(),
                            session_id,
                            agent_name,
                            task_title,
                            workspace_backend: Some(workspace_backend),
                            session: Some(session.clone()),
                            outcome: None,
                            current_cycle_index: None,
                            recent_activity: None,
                            latest_cycle: None,
                            latest_tool_call: None,
                            handle: None,
                            updated_at: now_millis(),
                            manager_listener_attached: true,
                        },
                    );
                    true
                }
            }
        };

        if should_attach_listener {
            let manager = self.clone();
            let task_id = task_id.clone();
            let listener: SubAgentSessionListener = Arc::new(move |event, payload| {
                manager.handle_session_event(&task_id, event, payload);
            });
            let _ = session.subscribe(listener);
        }
    }

    pub fn continue_task(&self, task_id: &str, prompt: &str) -> Result<(), String> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err("Follow-up prompt cannot be empty.".to_string());
        }

        let task_id = task_id.trim();
        let (session_id, agent_name) = {
            let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
            let Some(record) = tasks.get_mut(task_id) else {
                return Err(format!("Sub-task {task_id} not found."));
            };
            if record.is_running() {
                return Err(format!("Sub-task {task_id} is already running."));
            }
            if record
                .outcome
                .as_ref()
                .is_some_and(|outcome| outcome.status == AgentStatus::MaxCycles)
            {
                return Err(format!(
                    "Sub-task {task_id} reached max cycles and cannot continue."
                ));
            }
            if record.session_id.trim().is_empty() {
                return Err(format!("Sub-task {task_id} session is not available."));
            }
            if get_sub_agent_session(&record.session_id).is_none() {
                return Err(format!(
                    "Sub-task {task_id} session {} is not registered.",
                    record.session_id
                ));
            }

            record.task_title = prompt.to_string();
            record.outcome = None;
            record.recent_activity = Some(prompt.to_string());
            record.updated_at = now_millis();
            (record.session_id.clone(), record.agent_name.clone())
        };

        let tasks = self.tasks.clone();
        let task_id_for_thread = task_id.to_string();
        let prompt_for_thread = prompt.to_string();
        let session_id_for_thread = session_id.clone();
        let agent_name_for_thread = agent_name.clone();
        let handle = thread::spawn(move || {
            let outcome = continue_sub_agent_session(&session_id_for_thread, &prompt_for_thread)
                .unwrap_or_else(|error| SubTaskOutcome {
                    task_id: task_id_for_thread.clone(),
                    agent_name: agent_name_for_thread,
                    status: AgentStatus::Failed,
                    session_id: Some(session_id_for_thread),
                    final_answer: None,
                    wait_reason: None,
                    error: Some(error),
                    cycles: 0,
                    todo_list: Vec::new(),
                    resolved: BTreeMap::new(),
                });
            let mut tasks = tasks.lock().expect("sub-task manager poisoned");
            if let Some(record) = tasks.get_mut(&task_id_for_thread) {
                record.session_id = outcome
                    .session_id
                    .clone()
                    .unwrap_or_else(|| record.session_id.clone());
                record.agent_name = outcome.agent_name.clone();
                record.update_from_outcome(&outcome);
                record.outcome = Some(outcome);
                record.updated_at = now_millis();
                record.handle = None;
            }
        });

        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        if let Some(record) = tasks.get_mut(task_id) {
            record.handle = Some(handle);
            record.updated_at = now_millis();
        }
        Ok(())
    }

    pub fn status_entries(
        &self,
        task_ids: &[String],
        detail_level: &str,
        workspace_file_limit: usize,
    ) -> Vec<Value> {
        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        for record in tasks.values_mut() {
            if record.handle.as_ref().is_some_and(JoinHandle::is_finished) {
                if let Some(handle) = record.handle.take() {
                    let _ = handle.join();
                }
            }
        }
        task_ids
            .iter()
            .map(|task_id| {
                let Some(record) = tasks.get(task_id) else {
                    return json!({
                        "task_id": task_id,
                        "status": "missing",
                        "error": format!("Sub-task {task_id} not found."),
                    });
                };
                record.to_status_entry(detail_level, workspace_file_limit)
            })
            .collect()
    }

    pub fn task_session_id(&self, task_id: &str) -> Option<String> {
        self.join_finished_tasks();
        self.tasks
            .lock()
            .expect("sub-task manager poisoned")
            .get(task_id)
            .map(|record| record.session_id.clone())
    }

    pub fn task_status_label(&self, task_id: &str) -> Option<String> {
        self.join_finished_tasks();
        self.tasks
            .lock()
            .expect("sub-task manager poisoned")
            .get(task_id)
            .map(|record| record.status_label().to_string())
    }

    pub fn is_running(&self, task_id: &str) -> bool {
        self.join_finished_tasks();
        self.tasks
            .lock()
            .expect("sub-task manager poisoned")
            .get(task_id)
            .is_some_and(ManagedSubTask::is_running)
    }

    pub fn wait(&self, task_id: &str, timeout: Option<Duration>) -> bool {
        let deadline = timeout.map(|duration| Instant::now() + duration);
        loop {
            let handle = {
                let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
                let Some(record) = tasks.get_mut(task_id) else {
                    return false;
                };
                match record.handle.as_ref() {
                    Some(handle) if timeout.is_none() || handle.is_finished() => {
                        record.handle.take()
                    }
                    Some(_) => None,
                    None => return true,
                }
            };

            if let Some(handle) = handle {
                let _ = handle.join();
                return true;
            }

            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn join_finished_tasks(&self) {
        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        for record in tasks.values_mut() {
            if record.handle.as_ref().is_some_and(JoinHandle::is_finished) {
                if let Some(handle) = record.handle.take() {
                    let _ = handle.join();
                }
            }
        }
    }

    fn handle_session_event(&self, task_id: &str, event: &str, payload: &BTreeMap<String, Value>) {
        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        let Some(record) = tasks.get_mut(task_id) else {
            return;
        };
        record.updated_at = now_millis();
        match event {
            "session_run_start" => {
                if let Some(prompt) = preview_text(payload.get("prompt")) {
                    record.task_title = prompt.clone();
                    record.recent_activity = Some(prompt);
                }
            }
            "cycle_started" => {
                if let Some(cycle_index) = payload_u32(payload, "cycle") {
                    record.current_cycle_index = Some(cycle_index);
                    record.latest_cycle = Some(json!({
                        "cycle_index": cycle_index,
                        "status": "processing",
                    }));
                }
            }
            "cycle_llm_response" => {
                if let Some(cycle_index) = payload_u32(payload, "cycle") {
                    record.current_cycle_index = Some(cycle_index);
                }
                let assistant_preview = preview_text(
                    payload
                        .get("assistant_preview")
                        .or_else(|| payload.get("assistant_message")),
                );
                let mut latest_cycle = Map::new();
                latest_cycle.insert(
                    "status".to_string(),
                    Value::String("processing".to_string()),
                );
                if let Some(cycle_index) = record.current_cycle_index {
                    latest_cycle.insert("cycle_index".to_string(), Value::from(cycle_index));
                }
                if let Some(assistant_preview) = assistant_preview {
                    latest_cycle.insert(
                        "assistant_preview".to_string(),
                        Value::String(assistant_preview.clone()),
                    );
                    record.recent_activity = Some(assistant_preview);
                }
                record.latest_cycle = Some(Value::Object(latest_cycle));
            }
            "tool_result" => {
                let tool_status = preview_text(payload.get("status"));
                record.latest_tool_call = Some(json!({
                    "tool_call_id": payload.get("tool_call_id").cloned().unwrap_or(Value::Null),
                    "name": payload.get("tool_name").cloned().unwrap_or(Value::Null),
                    "status": tool_status,
                }));
                if record.recent_activity.is_none() {
                    record.recent_activity = preview_text(payload.get("tool_name"));
                }
            }
            "run_completed" => {
                record.mark_terminal_state(
                    "completed",
                    preview_text(payload.get("final_answer")).as_deref(),
                );
            }
            "run_wait_user" => {
                record.mark_terminal_state(
                    "wait_user",
                    preview_text(payload.get("wait_reason")).as_deref(),
                );
            }
            "run_max_cycles" => {
                let detail =
                    preview_text(payload.get("final_answer").or_else(|| payload.get("error")));
                record.mark_terminal_state("max_cycles", detail.as_deref());
            }
            "cycle_failed" => {
                record.mark_terminal_state("failed", preview_text(payload.get("error")).as_deref());
            }
            "session_run_end" => {
                if let Some(status) = preview_text(payload.get("status")) {
                    record.set_latest_cycle_status(&status);
                }
                let detail = preview_text(payload.get("final_answer"))
                    .or_else(|| preview_text(payload.get("wait_reason")))
                    .or_else(|| preview_text(payload.get("error")));
                if let Some(detail) = detail {
                    record.recent_activity = Some(detail);
                }
            }
            _ => {}
        }
    }
}

struct ManagedSubTask {
    task_id: String,
    session_id: String,
    agent_name: String,
    task_title: String,
    workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    session: Option<Arc<dyn SubAgentSession>>,
    outcome: Option<SubTaskOutcome>,
    current_cycle_index: Option<u32>,
    recent_activity: Option<String>,
    latest_cycle: Option<Value>,
    latest_tool_call: Option<Value>,
    handle: Option<JoinHandle<()>>,
    updated_at: u128,
    manager_listener_attached: bool,
}

impl ManagedSubTask {
    fn is_running(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }

    fn to_status_entry(&self, detail_level: &str, workspace_file_limit: usize) -> Value {
        let status = self.status_label();
        let mut entry = json!({
            "task_id": self.task_id,
            "session_id": self.session_id,
            "agent_name": self.agent_name,
            "status": status,
            "task_description": self.task_title,
        });
        if let Some(outcome) = &self.outcome {
            if let Some(final_answer) = &outcome.final_answer {
                entry["final_answer"] = Value::String(final_answer.clone());
            }
            if let Some(wait_reason) = &outcome.wait_reason {
                entry["wait_reason"] = Value::String(wait_reason.clone());
            }
            if let Some(error) = &outcome.error {
                entry["error"] = Value::String(error.clone());
            }
            if outcome.cycles > 0 {
                entry["cycles"] = Value::Number(outcome.cycles.into());
            }
            if !outcome.todo_list.is_empty() {
                entry["todo_list"] = Value::Array(outcome.todo_list.clone());
            }
            if !outcome.resolved.is_empty() {
                entry["resolved"] = json!(outcome.resolved);
            }
        }
        if detail_level == "snapshot" {
            let recent_activity = self
                .recent_activity
                .clone()
                .or_else(|| {
                    self.outcome.as_ref().and_then(|outcome| {
                        outcome
                            .final_answer
                            .clone()
                            .or_else(|| outcome.wait_reason.clone())
                            .or_else(|| outcome.error.clone())
                    })
                })
                .unwrap_or_else(|| self.task_title.clone());
            let workspace_snapshot = self.workspace_snapshot(workspace_file_limit);
            entry["snapshot"] = json!({
                "current_cycle_index": self.current_cycle_index,
                "task_title": self.task_title,
                "recent_activity": recent_activity,
                "updated_at": self.updated_at,
                "workspace_files": workspace_snapshot.files,
                "workspace_file_count": workspace_snapshot.file_count,
                "workspace_files_truncated": workspace_snapshot.truncated,
            });
            if let Some(latest_cycle) = &self.latest_cycle {
                entry["snapshot"]["latest_cycle"] = latest_cycle.clone();
            }
            if let Some(latest_tool_call) = &self.latest_tool_call {
                entry["snapshot"]["latest_tool_call"] = latest_tool_call.clone();
            }
        }
        entry
    }

    fn status_label(&self) -> &'static str {
        self.outcome
            .as_ref()
            .map(|outcome| status_label(outcome.status))
            .unwrap_or_else(|| {
                if self.is_running() {
                    "running"
                } else {
                    "pending"
                }
            })
    }

    fn update_from_outcome(&mut self, outcome: &SubTaskOutcome) {
        if outcome.cycles > 0 {
            self.current_cycle_index = Some(outcome.cycles);
        }
        self.set_latest_cycle_status(status_label(outcome.status));
        if let Some(detail) = outcome
            .final_answer
            .as_ref()
            .or(outcome.wait_reason.as_ref())
            .or(outcome.error.as_ref())
            .and_then(|value| preview_text(Some(&Value::String(value.clone()))))
        {
            self.recent_activity = Some(detail);
        }
    }

    fn set_latest_cycle_status(&mut self, status: &str) {
        let mut latest_cycle = self
            .latest_cycle
            .as_ref()
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        latest_cycle.insert("status".to_string(), Value::String(status.to_string()));
        if let Some(current_cycle_index) = self.current_cycle_index {
            latest_cycle
                .entry("cycle_index".to_string())
                .or_insert_with(|| Value::from(current_cycle_index));
        }
        self.latest_cycle = Some(Value::Object(latest_cycle));
    }

    fn mark_terminal_state(&mut self, status: &str, detail: Option<&str>) {
        self.set_latest_cycle_status(status);
        if let Some(detail) = detail {
            self.recent_activity = Some(detail.to_string());
        }
    }

    fn workspace_snapshot(&self, workspace_file_limit: usize) -> WorkspaceSnapshot {
        let Some(workspace_backend) = &self.workspace_backend else {
            return WorkspaceSnapshot::default();
        };
        let Ok(raw_files) = workspace_backend.list_files(".", "**/*") else {
            return WorkspaceSnapshot::default();
        };
        let visible_files = raw_files
            .into_iter()
            .filter(|path| !is_internal_workspace_file(path))
            .collect::<Vec<_>>();
        let file_count = visible_files.len();
        WorkspaceSnapshot {
            files: visible_files
                .into_iter()
                .take(workspace_file_limit)
                .collect::<Vec<_>>(),
            file_count,
            truncated: file_count > workspace_file_limit,
        }
    }
}

#[derive(Default)]
struct WorkspaceSnapshot {
    files: Vec<String>,
    file_count: usize,
    truncated: bool,
}

fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn payload_u32(payload: &BTreeMap<String, Value>, key: &str) -> Option<u32> {
    payload
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn preview_text(value: Option<&Value>) -> Option<String> {
    const PREVIEW_LIMIT: usize = 240;
    let text = match value? {
        Value::Null => return None,
        Value::String(value) => value.clone(),
        other => other.to_string(),
    };
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.len() <= PREVIEW_LIMIT {
        return Some(text.to_string());
    }
    let mut truncated = text
        .chars()
        .take(PREVIEW_LIMIT.saturating_sub(3))
        .collect::<String>();
    truncated = truncated.trim_end().to_string();
    truncated.push_str("...");
    Some(truncated)
}

fn is_internal_workspace_file(path: &str) -> bool {
    let normalized = path.trim().trim_matches('/');
    normalized.is_empty()
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.'))
}
