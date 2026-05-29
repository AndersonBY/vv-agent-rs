use serde_json::Value;

use crate::runtime::sub_task_manager::SubTaskManager;
use crate::types::{AgentTask, SubTaskRequest};

pub(super) struct SubTaskIdentity {
    pub(super) task_id: String,
    pub(super) session_id: String,
}

pub(super) fn resolve_sub_task_identity(
    parent_task: &AgentTask,
    request: &SubTaskRequest,
) -> SubTaskIdentity {
    let task_id = request
        .metadata
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            SubTaskManager::next_task_identity(&parent_task.task_id, &request.agent_name).0
        });
    let session_id = request
        .metadata
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| task_id.clone());

    SubTaskIdentity {
        task_id,
        session_id,
    }
}
