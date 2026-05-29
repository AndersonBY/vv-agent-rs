use std::collections::BTreeMap;

use crate::types::{AgentStatus, SubTaskOutcome};

use super::super::types::SubTaskRunContext;

pub(super) fn failed_sub_task_outcome(
    task_id: &str,
    agent_name: &str,
    session_id: &str,
    error: impl Into<String>,
) -> SubTaskOutcome {
    SubTaskOutcome {
        task_id: task_id.to_string(),
        agent_name: agent_name.to_string(),
        status: AgentStatus::Failed,
        session_id: Some(session_id.to_string()),
        final_answer: None,
        wait_reason: None,
        error: Some(error.into()),
        cycles: 0,
        todo_list: Vec::new(),
        resolved: BTreeMap::new(),
    }
}

pub(super) fn record_sub_task_outcome(
    context: &SubTaskRunContext,
    task_id: &str,
    outcome: SubTaskOutcome,
) -> SubTaskOutcome {
    context
        .sub_task_manager
        .record_outcome(task_id, outcome.clone());
    outcome
}
