use std::collections::BTreeMap;

use crate::types::{AgentStatus, SubTaskOutcome};
use crate::workspace::WorkspaceBackend;
use std::sync::Arc;

use super::super::types::{SubRunLifecycle, SubTaskRunContext};
use crate::runtime::sub_task_manager::SubTaskLineage;

pub(super) fn failed_sub_task_outcome(
    task_id: &str,
    agent_name: &str,
    session_id: &str,
    error: impl Into<String>,
) -> SubTaskOutcome {
    failed_sub_task_outcome_with_code(task_id, agent_name, session_id, error, None)
}

pub(super) fn failed_sub_task_outcome_with_code(
    task_id: &str,
    agent_name: &str,
    session_id: &str,
    error: impl Into<String>,
    error_code: Option<&str>,
) -> SubTaskOutcome {
    SubTaskOutcome {
        task_id: task_id.to_string(),
        agent_name: agent_name.to_string(),
        status: AgentStatus::Failed,
        session_id: Some(session_id.to_string()),
        final_answer: None,
        wait_reason: None,
        error: Some(error.into()),
        error_code: Some(error_code.unwrap_or("sub_task_failed").to_string()),
        cycles: 0,
        todo_list: Vec::new(),
        resolved: BTreeMap::new(),
    }
}

pub(super) fn record_sub_task_outcome(
    context: &SubTaskRunContext,
    lifecycle: &SubRunLifecycle,
    workspace_backend: Arc<dyn WorkspaceBackend>,
    outcome: SubTaskOutcome,
) -> SubTaskOutcome {
    context.sub_task_manager.record_outcome_with_context(
        &lifecycle.task_id,
        outcome.clone(),
        Some(workspace_backend),
        SubTaskLineage {
            parent_run_id: (!lifecycle.parent_run_id.is_empty())
                .then(|| lifecycle.parent_run_id.clone()),
            parent_tool_call_id: (!lifecycle.parent_tool_call_id.is_empty())
                .then(|| lifecycle.parent_tool_call_id.clone()),
        },
    );
    outcome
}
