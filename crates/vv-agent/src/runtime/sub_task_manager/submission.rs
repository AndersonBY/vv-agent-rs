use std::collections::BTreeMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::thread;

use crate::types::{AgentStatus, SubTaskOutcome};
use crate::workspace::WorkspaceBackend;

use super::helpers::{normalize_failed_outcome, now_iso, panic_payload_to_string};
use super::manager::SubTaskManager;
use super::record::ManagedSubTask;
use super::types::{SubTaskLineage, SubTaskSubmissionContext};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubTaskSubmitError {
    AlreadyRunning { task_id: String },
    SpawnFailed { task_id: String, error: String },
}

impl SubTaskSubmitError {
    pub(crate) fn error_code(&self) -> &'static str {
        match self {
            Self::AlreadyRunning { .. } => "sub_task_already_running",
            Self::SpawnFailed { .. } => "sub_task_submit_failed",
        }
    }
}

impl fmt::Display for SubTaskSubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning { task_id } => {
                write!(formatter, "Sub-task {task_id} is already running.")
            }
            Self::SpawnFailed { task_id, error } => {
                write!(
                    formatter,
                    "Sub-task {task_id} thread failed to spawn: {error}"
                )
            }
        }
    }
}

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
        self.submit_with_context(
            task_id,
            session_id,
            agent_name,
            task_title,
            SubTaskSubmissionContext {
                workspace_backend,
                lineage: SubTaskLineage::default(),
            },
            runner,
        )
    }

    pub fn submit_with_context(
        &self,
        task_id: impl Into<String>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        task_title: impl Into<String>,
        context: SubTaskSubmissionContext,
        runner: impl FnOnce() -> SubTaskOutcome + Send + 'static,
    ) -> Result<(), String> {
        self.submit_with_context_detailed(
            task_id, session_id, agent_name, task_title, context, runner,
        )
        .map_err(|error| error.to_string())
    }

    pub(crate) fn submit_with_context_detailed(
        &self,
        task_id: impl Into<String>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        task_title: impl Into<String>,
        context: SubTaskSubmissionContext,
        runner: impl FnOnce() -> SubTaskOutcome + Send + 'static,
    ) -> Result<(), SubTaskSubmitError> {
        let SubTaskSubmissionContext {
            workspace_backend,
            lineage,
        } = context;
        let task_id = task_id.into();
        let session_id = session_id.into();
        let agent_name = agent_name.into();
        let task_title = task_title.into();
        let mut task_records = self.tasks.lock().expect("sub-task manager poisoned");
        if task_records
            .get(&task_id)
            .is_some_and(ManagedSubTask::is_running)
        {
            return Err(SubTaskSubmitError::AlreadyRunning { task_id });
        }
        let previous_record = task_records.insert(
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
                parent_run_id: lineage.parent_run_id,
                parent_tool_call_id: lineage.parent_tool_call_id,
                running: true,
                worker_owned_run: true,
                handle: None,
                updated_at: now_iso(),
                session_generation: 0,
                manager_listener_generation: None,
            },
        );

        let tasks = self.tasks.clone();
        let task_id_for_thread = task_id.clone();
        let spawn_result = catch_unwind(AssertUnwindSafe(|| {
            thread::Builder::new()
                .name(format!("vv-agent-sub-task-{session_id}"))
                .spawn(move || {
                    let outcome = catch_unwind(AssertUnwindSafe(runner));
                    let mut tasks = tasks.lock().expect("sub-task manager poisoned");
                    if let Some(record) = tasks.get_mut(&task_id_for_thread) {
                        let outcome = normalize_failed_outcome(match outcome {
                            Ok(outcome) => outcome,
                            Err(payload) => SubTaskOutcome {
                                task_id: record.task_id.clone(),
                                agent_name: record.agent_name.clone(),
                                status: AgentStatus::Failed,
                                session_id: Some(record.session_id.clone()),
                                final_answer: None,
                                wait_reason: None,
                                error: Some(panic_payload_to_string(payload.as_ref())),
                                error_code: Some("sub_task_failed".to_string()),
                                cycles: 0,
                                todo_list: Vec::new(),
                                resolved: record.resolved.clone(),
                            },
                        });
                        if !outcome.resolved.is_empty() {
                            record.resolved = outcome.resolved.clone();
                        }
                        record.update_from_outcome(&outcome);
                        record.outcome = Some(outcome);
                        record.updated_at = now_iso();
                        record.running = false;
                        record.worker_owned_run = false;
                    }
                })
        }));
        let handle = match spawn_result {
            Ok(Ok(handle)) => handle,
            Ok(Err(error)) => {
                restore_after_spawn_failure(&mut task_records, &task_id, previous_record);
                return Err(SubTaskSubmitError::SpawnFailed {
                    task_id,
                    error: error.to_string(),
                });
            }
            Err(payload) => {
                let error = panic_payload_to_string(payload.as_ref());
                restore_after_spawn_failure(&mut task_records, &task_id, previous_record);
                return Err(SubTaskSubmitError::SpawnFailed { task_id, error });
            }
        };

        if let Some(record) = task_records.get_mut(&task_id) {
            record.handle = Some(handle);
            record.updated_at = now_iso();
        }
        Ok(())
    }

    pub fn record_outcome(&self, task_id: &str, outcome: SubTaskOutcome) {
        self.record_outcome_with_context(task_id, outcome, None, SubTaskLineage::default());
    }

    pub fn record_outcome_with_context(
        &self,
        task_id: &str,
        outcome: SubTaskOutcome,
        workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
        lineage: SubTaskLineage,
    ) {
        let outcome = normalize_failed_outcome(outcome);
        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        let task_id = task_id.to_string();
        match tasks.get_mut(&task_id) {
            Some(record) => {
                if workspace_backend.is_some() {
                    record.workspace_backend = workspace_backend;
                }
                if lineage.parent_run_id.is_some() {
                    record.parent_run_id = lineage.parent_run_id;
                }
                if lineage.parent_tool_call_id.is_some() {
                    record.parent_tool_call_id = lineage.parent_tool_call_id;
                }
                record.session_id = outcome
                    .session_id
                    .clone()
                    .unwrap_or_else(|| record.session_id.clone());
                record.agent_name = outcome.agent_name.clone();
                if !outcome.resolved.is_empty() {
                    record.resolved = outcome.resolved.clone();
                }
                if record.running && record.worker_owned_run {
                    record.updated_at = now_iso();
                    return;
                }
                record.running = false;
                record.worker_owned_run = false;
                record.update_from_outcome(&outcome);
                record.outcome = Some(outcome);
                record.updated_at = now_iso();
            }
            None => {
                let mut record = ManagedSubTask {
                    session_id: outcome.session_id.clone().unwrap_or_default(),
                    agent_name: outcome.agent_name.clone(),
                    task_title: String::new(),
                    workspace_backend,
                    session: None,
                    outcome: None,
                    resolved: outcome.resolved.clone(),
                    current_cycle_index: None,
                    recent_activity: None,
                    latest_cycle: None,
                    latest_tool_call: None,
                    parent_run_id: lineage.parent_run_id,
                    parent_tool_call_id: lineage.parent_tool_call_id,
                    task_id: task_id.clone(),
                    running: false,
                    worker_owned_run: false,
                    handle: None,
                    updated_at: now_iso(),
                    session_generation: 0,
                    manager_listener_generation: None,
                };
                record.update_from_outcome(&outcome);
                record.outcome = Some(outcome);
                tasks.insert(task_id.clone(), record);
            }
        }
    }
}

fn restore_after_spawn_failure(
    task_records: &mut BTreeMap<String, ManagedSubTask>,
    task_id: &str,
    previous_record: Option<ManagedSubTask>,
) {
    task_records.remove(task_id);
    if let Some(previous_record) = previous_record {
        task_records.insert(task_id.to_string(), previous_record);
    }
}
