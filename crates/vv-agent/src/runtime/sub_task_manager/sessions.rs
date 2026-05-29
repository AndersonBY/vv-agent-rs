use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::thread;

use crate::runtime::sub_agent_sessions::{
    register_sub_agent_session, unregister_sub_agent_session, SubAgentSession,
    SubAgentSessionListener,
};
use crate::types::{AgentStatus, SubTaskOutcome};
use crate::workspace::WorkspaceBackend;

use super::helpers::{now_iso, panic_payload_to_string};
use super::manager::SubTaskManager;
use super::record::ManagedSubTask;
use super::types::SubTaskSessionAttachment;

impl SubTaskManager {
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
}
