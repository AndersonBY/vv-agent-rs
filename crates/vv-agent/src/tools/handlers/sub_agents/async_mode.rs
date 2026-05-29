use serde_json::{json, Value};

use crate::runtime::sub_task_manager::SubTaskManager;
use crate::tools::base::{SubTaskRunner, ToolContext};
use crate::tools::common::tool_error_with_code;
use crate::types::{SubTaskRequest, ToolExecutionResult};

use super::request::BatchRequestEntry;
use super::response;

pub(super) fn start_single_async(
    context: &mut ToolContext,
    runner: SubTaskRunner,
    mut request: SubTaskRequest,
) -> ToolExecutionResult {
    let Some(manager) = context.sub_task_manager.clone() else {
        return tool_error_with_code(
            "Sub-task manager is not available for async mode",
            "sub_task_manager_unavailable",
        );
    };

    let (task_id, session_id) =
        SubTaskManager::next_task_identity(&context.task_id, &request.agent_name);
    request
        .metadata
        .insert("task_id".to_string(), Value::String(task_id.clone()));
    request
        .metadata
        .insert("session_id".to_string(), Value::String(session_id.clone()));
    let agent_name = request.agent_name.clone();
    let task_description = request.task_description.clone();
    if let Err(error) = manager.submit_with_workspace(
        task_id.clone(),
        session_id.clone(),
        agent_name.clone(),
        task_description.clone(),
        Some(context.workspace_backend.clone()),
        move || runner(request),
    ) {
        return tool_error_with_code(error, "sub_task_already_running");
    }

    response::success(json!({
        "task_id": task_id,
        "session_id": session_id,
        "agent_name": agent_name,
        "status": "running",
        "task_description": task_description,
        "wait_for_completion": false,
    }))
}

pub(super) fn start_batch_async(
    context: &mut ToolContext,
    runner: SubTaskRunner,
    agent_name: &str,
    total: usize,
    entries: Vec<BatchRequestEntry>,
) -> ToolExecutionResult {
    let Some(manager) = context.sub_task_manager.clone() else {
        return tool_error_with_code(
            "Sub-task manager is not available for async mode",
            "sub_task_manager_unavailable",
        );
    };

    let mut results = Vec::new();
    let mut task_ids = Vec::new();
    let mut started = 0usize;
    let mut failed = 0usize;
    for entry in entries {
        let Some(mut request) = entry.request else {
            failed += 1;
            results.push(json!({
                "index": entry.index,
                "status": "failed",
                "error": entry.error.unwrap_or_else(|| "Invalid task item".to_string()),
            }));
            continue;
        };
        let (task_id, session_id) =
            SubTaskManager::next_task_identity(&context.task_id, agent_name);
        request
            .metadata
            .insert("task_id".to_string(), Value::String(task_id.clone()));
        request
            .metadata
            .insert("session_id".to_string(), Value::String(session_id.clone()));
        let task_title = request.task_description.clone();
        let runner = runner.clone();
        if let Err(error) = manager.submit_with_workspace(
            task_id.clone(),
            session_id.clone(),
            agent_name.to_string(),
            task_title.clone(),
            Some(context.workspace_backend.clone()),
            move || runner(request),
        ) {
            results.push(json!({
                "index": entry.index,
                "task_id": task_id,
                "session_id": session_id,
                "agent_name": agent_name,
                "status": "failed",
                "error": error,
            }));
            continue;
        }
        started += 1;
        task_ids.push(task_id.clone());
        results.push(json!({
            "index": entry.index,
            "task_id": task_id,
            "session_id": session_id,
            "agent_name": agent_name,
            "status": "running",
            "task_description": task_title,
        }));
    }

    response::success(json!({
        "summary": {
            "total": total,
            "accepted": started,
            "failed": failed,
        },
        "task_ids": task_ids,
        "results": results,
        "wait_for_completion": false,
    }))
}
