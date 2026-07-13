use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::thread;

use crate::runtime::sub_agent_sessions::{
    register_sub_agent_session, sub_agent_session_registry, SubAgentSession,
    SubAgentSessionListener,
};
use crate::tools::common::trim_portable_whitespace;
use crate::types::{AgentStatus, SubTaskOutcome};
use crate::workspace::WorkspaceBackend;

use super::helpers::{normalize_failed_outcome, now_iso, panic_payload_to_string};
use super::manager::SubTaskManager;
use super::record::{ManagedSubAgentSession, ManagedSubTask};
use super::types::{SubTaskLineage, SubTaskSessionAttachment, SubTaskTurnSnapshot};

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
        self.attach_session_with_resolved_and_lineage(attachment, SubTaskLineage::default());
    }

    pub fn attach_session_with_resolved_and_lineage(
        &self,
        attachment: SubTaskSessionAttachment,
        lineage: SubTaskLineage,
    ) {
        self.attach_session_inner(attachment, lineage, false);
    }

    pub(crate) fn attach_running_session_with_resolved_and_lineage(
        &self,
        attachment: SubTaskSessionAttachment,
        lineage: SubTaskLineage,
    ) {
        self.attach_session_inner(attachment, lineage, true);
    }

    fn attach_session_inner(
        &self,
        attachment: SubTaskSessionAttachment,
        lineage: SubTaskLineage,
        running: bool,
    ) {
        let SubTaskSessionAttachment {
            task_id,
            session_id,
            agent_name,
            task_title,
            workspace_backend,
            session,
            resolved,
        } = attachment;
        let listener_generation = {
            let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
            match tasks.get_mut(&task_id) {
                Some(record) => {
                    let initial_lineage = record
                        .session
                        .as_ref()
                        .map(|attached| attached.initial_lineage.clone())
                        .unwrap_or_else(|| SubTaskLineage {
                            parent_run_id: lineage
                                .parent_run_id
                                .clone()
                                .or_else(|| record.parent_run_id.clone()),
                            parent_tool_call_id: lineage
                                .parent_tool_call_id
                                .clone()
                                .or_else(|| record.parent_tool_call_id.clone()),
                        });
                    let session_changed = record
                        .session
                        .as_ref()
                        .is_none_or(|attached| !Arc::ptr_eq(&attached.session, &session));
                    if session_changed {
                        record.session_generation =
                            record.session_generation.saturating_add(1).max(1);
                        record.manager_listener_generation = None;
                    } else if record.session_generation == 0 {
                        record.session_generation = 1;
                    }
                    record.session_id = session_id;
                    record.agent_name = agent_name;
                    if !task_title.is_empty() {
                        record.task_title = task_title;
                    }
                    record.workspace_backend = Some(workspace_backend);
                    record.session = Some(ManagedSubAgentSession {
                        session: session.clone(),
                        initial_lineage,
                    });
                    if !resolved.is_empty() {
                        record.resolved = resolved;
                    }
                    if lineage.parent_run_id.is_some() {
                        record.parent_run_id = lineage.parent_run_id;
                    }
                    if lineage.parent_tool_call_id.is_some() {
                        record.parent_tool_call_id = lineage.parent_tool_call_id;
                    }
                    if running {
                        record.running = true;
                        record.outcome = None;
                    }
                    record.updated_at = now_iso();
                    (record.manager_listener_generation != Some(record.session_generation))
                        .then_some(record.session_generation)
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
                            session: Some(ManagedSubAgentSession {
                                session: session.clone(),
                                initial_lineage: lineage.clone(),
                            }),
                            outcome: None,
                            resolved,
                            current_cycle_index: None,
                            recent_activity: None,
                            latest_cycle: None,
                            latest_tool_call: None,
                            parent_run_id: lineage.parent_run_id,
                            parent_tool_call_id: lineage.parent_tool_call_id,
                            running,
                            worker_owned_run: false,
                            handle: None,
                            updated_at: now_iso(),
                            session_generation: 1,
                            manager_listener_generation: None,
                        },
                    );
                    Some(1)
                }
            }
        };

        if let Some(session_generation) = listener_generation {
            let tasks = Arc::downgrade(&self.tasks);
            let listener_task_id = task_id.clone();
            let listener: SubAgentSessionListener = Arc::new(move |event, payload| {
                let Some(tasks) = tasks.upgrade() else {
                    return;
                };
                SubTaskManager { tasks }.handle_session_event(
                    &listener_task_id,
                    session_generation,
                    event,
                    payload,
                );
            });
            let _ = session.subscribe(listener);
            let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
            if let Some(record) = tasks.get_mut(&task_id) {
                let session_is_current = record
                    .session
                    .as_ref()
                    .is_some_and(|attached| Arc::ptr_eq(&attached.session, &session));
                if session_is_current && record.session_generation == session_generation {
                    record.manager_listener_generation = Some(session_generation);
                }
            }
        }
    }

    pub fn continue_task(&self, task_id: &str, prompt: &str) -> Result<(), String> {
        self.continue_task_inner(task_id, prompt, None)
    }

    pub(crate) fn continue_task_with_snapshot(
        &self,
        task_id: &str,
        prompt: &str,
        snapshot: SubTaskTurnSnapshot,
    ) -> Result<(), String> {
        self.continue_task_inner(task_id, prompt, Some(snapshot))
    }

    fn continue_task_inner(
        &self,
        task_id: &str,
        prompt: &str,
        snapshot: Option<SubTaskTurnSnapshot>,
    ) -> Result<(), String> {
        let prompt = trim_portable_whitespace(prompt);
        if prompt.is_empty() {
            return Err("Follow-up prompt cannot be empty.".to_string());
        }

        let (
            session_id,
            agent_name,
            session,
            resolved,
            session_generation,
            previous,
            mut registration,
        ) = {
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
                return Err(format!("Sub-task {task_id} session is not attached."));
            }
            let Some(session) = record.session.clone() else {
                return Err(format!("Sub-task {task_id} session is not attached."));
            };
            let continuation_lineage = snapshot
                .as_ref()
                .map(|snapshot| SubTaskLineage {
                    parent_run_id: snapshot.parent_run_id.clone(),
                    parent_tool_call_id: snapshot.parent_tool_call_id.clone(),
                })
                .unwrap_or_else(|| session.initial_lineage.clone());
            let registration = ContinuationRegistration::register(
                record.session_id.clone(),
                session.session.clone(),
            );
            let previous = record.admit_continuation(prompt, &continuation_lineage, now_iso());
            (
                record.session_id.clone(),
                record.agent_name.clone(),
                session,
                record.resolved.clone(),
                record.session_generation,
                previous,
                registration,
            )
        };

        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| {
            session.session.sanitize_for_resume();
        })) {
            let error = panic_payload_to_string(payload.as_ref());
            let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
            if let Some(record) = tasks.get_mut(task_id) {
                record.rollback_continuation(previous);
            }
            return Err(format!(
                "Sub-task {task_id} continuation setup failed: {error}"
            ));
        }

        let tasks = self.tasks.clone();
        let task_id_for_thread = task_id.to_string();
        let prompt_for_thread = prompt.to_string();
        let session_id_for_thread = session_id.clone();
        let agent_name_for_thread = agent_name.clone();
        let session_generation_for_thread = session_generation;
        let turn_event_handler = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.event_handler.clone());
        let session_for_thread = session.session.clone();
        let mut task_records = self.tasks.lock().expect("sub-task manager poisoned");
        let spawn_result = catch_unwind(AssertUnwindSafe(|| {
            thread::Builder::new()
                .name(format!("vv-agent-sub-task-{session_id_for_thread}"))
                .spawn(move || {
                    let _event_handler_scope =
                        SubTaskTurnSnapshot::enter_event_handler_scope(turn_event_handler);
                    let worker_result = catch_unwind(AssertUnwindSafe(|| match snapshot {
                        Some(snapshot) => session_for_thread
                            .continue_run_with_snapshot(&prompt_for_thread, snapshot),
                        None => session_for_thread.continue_run(&prompt_for_thread),
                    }));
                    let outcome = normalize_failed_outcome(match worker_result {
                        Ok(Ok(outcome)) => outcome,
                        Ok(Err(error)) => SubTaskOutcome {
                            task_id: task_id_for_thread.clone(),
                            agent_name: agent_name_for_thread.clone(),
                            status: AgentStatus::Failed,
                            session_id: Some(session_id_for_thread.clone()),
                            final_answer: None,
                            wait_reason: None,
                            error: Some(error),
                            error_code: Some("sub_task_failed".to_string()),
                            cycles: 0,
                            todo_list: Vec::new(),
                            resolved: resolved.clone(),
                        },
                        Err(payload) => SubTaskOutcome {
                            task_id: task_id_for_thread.clone(),
                            agent_name: agent_name_for_thread.clone(),
                            status: AgentStatus::Failed,
                            session_id: Some(session_id_for_thread.clone()),
                            final_answer: None,
                            wait_reason: None,
                            error: Some(panic_payload_to_string(payload.as_ref())),
                            error_code: Some("sub_task_failed".to_string()),
                            cycles: 0,
                            todo_list: Vec::new(),
                            resolved,
                        },
                    });
                    {
                        let mut tasks = tasks
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if let Some(record) = tasks.get_mut(&task_id_for_thread) {
                            let owns_session_generation = record.session_generation
                                == session_generation_for_thread
                                && record.session.as_ref().is_some_and(|attached| {
                                    Arc::ptr_eq(&attached.session, &session_for_thread)
                                });
                            let mut outcome = outcome;
                            if outcome.resolved.is_empty() && !record.resolved.is_empty() {
                                outcome.resolved = record.resolved.clone();
                            }
                            if owns_session_generation {
                                record.session_id = outcome
                                    .session_id
                                    .clone()
                                    .unwrap_or_else(|| record.session_id.clone());
                                record.agent_name = outcome.agent_name.clone();
                                record.update_from_outcome(&outcome);
                                record.outcome = Some(outcome);
                                record.running = false;
                                record.worker_owned_run = false;
                                record.updated_at = now_iso();
                            }
                        }
                    }
                    sub_agent_session_registry()
                        .unregister_if_matches(&session_id_for_thread, Some(&session_for_thread));
                })
        }));
        let handle = match spawn_result {
            Ok(Ok(handle)) => handle,
            Ok(Err(error)) => {
                if let Some(record) = task_records.get_mut(task_id) {
                    record.rollback_continuation(previous);
                }
                return Err(format!(
                    "Sub-task {task_id} continuation thread failed to spawn: {error}"
                ));
            }
            Err(payload) => {
                let error = panic_payload_to_string(payload.as_ref());
                if let Some(record) = task_records.get_mut(task_id) {
                    record.rollback_continuation(previous);
                }
                return Err(format!(
                    "Sub-task {task_id} continuation thread failed to spawn: {error}"
                ));
            }
        };

        if let Some(record) = task_records.get_mut(task_id) {
            record.handle = Some(handle);
            record.updated_at = now_iso();
        }
        registration.commit();
        Ok(())
    }
}

struct ContinuationRegistration {
    session_id: String,
    session: Arc<dyn SubAgentSession>,
    previous: Option<Arc<dyn SubAgentSession>>,
    committed: bool,
}

impl ContinuationRegistration {
    fn register(session_id: String, session: Arc<dyn SubAgentSession>) -> Self {
        let previous = sub_agent_session_registry().get(&session_id);
        register_sub_agent_session(session_id.clone(), session.clone());
        Self {
            session_id,
            session,
            previous,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for ContinuationRegistration {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let removed = sub_agent_session_registry()
            .unregister_if_matches(&self.session_id, Some(&self.session));
        if removed {
            if let Some(previous) = self.previous.clone() {
                register_sub_agent_session(self.session_id.clone(), previous);
            }
        }
    }
}
