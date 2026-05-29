use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::runtime::sub_agent_sessions::{
    register_sub_agent_session, unregister_sub_agent_session, SubAgentSession,
    SubAgentSessionListener,
};
use crate::types::{AgentStatus, SubTaskOutcome};
use crate::workspace::WorkspaceBackend;

use super::helpers::{now_iso, panic_payload_to_string};
use super::record::ManagedSubTask;
use super::types::{ManagedSubTaskSnapshot, SubTaskSessionAttachment};

static SUB_TASK_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Default)]
pub struct SubTaskManager {
    pub(super) tasks: Arc<Mutex<BTreeMap<String, ManagedSubTask>>>,
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
    ) -> Result<(), String> {
        self.submit_with_workspace(task_id, session_id, agent_name, task_title, None, runner)
    }

    pub fn submit_with_workspace(
        &self,
        task_id: impl Into<String>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        task_title: impl Into<String>,
        workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
        runner: impl FnOnce() -> SubTaskOutcome + Send + 'static,
    ) -> Result<(), String> {
        let task_id = task_id.into();
        let session_id = session_id.into();
        let agent_name = agent_name.into();
        let task_title = task_title.into();
        {
            let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
            if tasks.get(&task_id).is_some_and(ManagedSubTask::is_running) {
                return Err(format!("Sub-task {task_id} is already running."));
            }
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
                    resolved: BTreeMap::new(),
                    current_cycle_index: None,
                    recent_activity: None,
                    latest_cycle: None,
                    latest_tool_call: None,
                    handle: None,
                    updated_at: now_iso(),
                    manager_listener_attached: false,
                },
            );
        }

        let tasks = self.tasks.clone();
        let task_id_for_thread = task_id.clone();
        let handle = thread::spawn(move || {
            let outcome = catch_unwind(AssertUnwindSafe(runner));
            let mut tasks = tasks.lock().expect("sub-task manager poisoned");
            if let Some(record) = tasks.get_mut(&task_id_for_thread) {
                let outcome = match outcome {
                    Ok(outcome) => outcome,
                    Err(payload) => SubTaskOutcome {
                        task_id: record.task_id.clone(),
                        agent_name: record.agent_name.clone(),
                        status: AgentStatus::Failed,
                        session_id: Some(record.session_id.clone()),
                        final_answer: None,
                        wait_reason: None,
                        error: Some(panic_payload_to_string(payload.as_ref())),
                        cycles: 0,
                        todo_list: Vec::new(),
                        resolved: record.resolved.clone(),
                    },
                };
                if !outcome.resolved.is_empty() {
                    record.resolved = outcome.resolved.clone();
                }
                record.update_from_outcome(&outcome);
                record.outcome = Some(outcome);
                record.updated_at = now_iso();
                record.handle = None;
            }
        });

        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        if let Some(record) = tasks.get_mut(&task_id) {
            record.handle = Some(handle);
            record.updated_at = now_iso();
        }
        Ok(())
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
                if !outcome.resolved.is_empty() {
                    record.resolved = outcome.resolved.clone();
                }
                record.update_from_outcome(&outcome);
                record.outcome = Some(outcome);
                record.updated_at = now_iso();
            }
            None => {
                let mut record = ManagedSubTask {
                    session_id: outcome.session_id.clone().unwrap_or_default(),
                    agent_name: outcome.agent_name.clone(),
                    task_title: String::new(),
                    workspace_backend: None,
                    session: None,
                    outcome: None,
                    resolved: outcome.resolved.clone(),
                    current_cycle_index: None,
                    recent_activity: None,
                    latest_cycle: None,
                    latest_tool_call: None,
                    task_id: task_id.clone(),
                    handle: None,
                    updated_at: now_iso(),
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
        self.attach_session_with_resolved(SubTaskSessionAttachment {
            task_id: task_id.into(),
            session_id: session_id.into(),
            agent_name: agent_name.into(),
            task_title: task_title.into(),
            workspace_backend,
            session,
            resolved: BTreeMap::new(),
        });
    }

    pub fn attach_session_with_resolved(&self, attachment: SubTaskSessionAttachment) {
        let SubTaskSessionAttachment {
            task_id,
            session_id,
            agent_name,
            task_title,
            workspace_backend,
            session,
            resolved,
        } = attachment;
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
                    if !resolved.is_empty() {
                        record.resolved = resolved;
                    }
                    record.updated_at = now_iso();
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
                            resolved,
                            current_cycle_index: None,
                            recent_activity: None,
                            latest_cycle: None,
                            latest_tool_call: None,
                            handle: None,
                            updated_at: now_iso(),
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
        let (session_id, agent_name, session) = {
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
            let Some(session) = record.session.clone() else {
                return Err(format!("Sub-task {task_id} session is not attached."));
            };

            record.task_title = prompt.to_string();
            record.outcome = None;
            record.recent_activity = Some(prompt.to_string());
            record.updated_at = now_iso();
            (
                record.session_id.clone(),
                record.agent_name.clone(),
                session,
            )
        };

        session.sanitize_for_resume();

        let tasks = self.tasks.clone();
        let task_id_for_thread = task_id.to_string();
        let prompt_for_thread = prompt.to_string();
        let session_id_for_thread = session_id.clone();
        let agent_name_for_thread = agent_name.clone();
        let handle = thread::spawn(move || {
            register_sub_agent_session(session_id_for_thread.clone(), session.clone());
            let outcome = match catch_unwind(AssertUnwindSafe(|| {
                session.continue_run(&prompt_for_thread)
            })) {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(error)) => SubTaskOutcome {
                    task_id: task_id_for_thread.clone(),
                    agent_name: agent_name_for_thread.clone(),
                    status: AgentStatus::Failed,
                    session_id: Some(session_id_for_thread.clone()),
                    final_answer: None,
                    wait_reason: None,
                    error: Some(error),
                    cycles: 0,
                    todo_list: Vec::new(),
                    resolved: BTreeMap::new(),
                },
                Err(payload) => SubTaskOutcome {
                    task_id: task_id_for_thread.clone(),
                    agent_name: agent_name_for_thread.clone(),
                    status: AgentStatus::Failed,
                    session_id: Some(session_id_for_thread.clone()),
                    final_answer: None,
                    wait_reason: None,
                    error: Some(panic_payload_to_string(payload.as_ref())),
                    cycles: 0,
                    todo_list: Vec::new(),
                    resolved: BTreeMap::new(),
                },
            };
            unregister_sub_agent_session(&session_id_for_thread);
            let mut tasks = tasks.lock().expect("sub-task manager poisoned");
            if let Some(record) = tasks.get_mut(&task_id_for_thread) {
                let mut outcome = outcome;
                if outcome.resolved.is_empty() && !record.resolved.is_empty() {
                    outcome.resolved = record.resolved.clone();
                }
                record.session_id = outcome
                    .session_id
                    .clone()
                    .unwrap_or_else(|| record.session_id.clone());
                record.agent_name = outcome.agent_name.clone();
                record.update_from_outcome(&outcome);
                record.outcome = Some(outcome);
                record.updated_at = now_iso();
                record.handle = None;
            }
        });

        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        if let Some(record) = tasks.get_mut(task_id) {
            record.handle = Some(handle);
            record.updated_at = now_iso();
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

    pub fn get(&self, task_id: &str) -> Option<ManagedSubTaskSnapshot> {
        self.join_finished_tasks();
        self.tasks
            .lock()
            .expect("sub-task manager poisoned")
            .get(task_id)
            .map(ManagedSubTask::snapshot)
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

    pub fn wait_for_record(
        &self,
        task_id: &str,
        timeout: Option<Duration>,
    ) -> Option<ManagedSubTaskSnapshot> {
        if !self.wait(task_id, timeout) {
            return None;
        }
        self.get(task_id)
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
}
