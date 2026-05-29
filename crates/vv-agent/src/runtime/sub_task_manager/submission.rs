use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::thread;

use crate::types::{AgentStatus, SubTaskOutcome};
use crate::workspace::WorkspaceBackend;

use super::helpers::{now_iso, panic_payload_to_string};
use super::manager::SubTaskManager;
use super::record::ManagedSubTask;

impl SubTaskManager {
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
}
