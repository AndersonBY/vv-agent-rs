use std::sync::Arc;

use serde_json::{json, Value};

use crate::runtime::sub_task_manager::{SubTaskLineage, SubTaskManager, SubTaskSubmissionContext};
use crate::runtime::with_assigned_sub_task_identity;
use crate::tools::base::{SubTaskRunner, ToolContext};
use crate::types::{SubTaskRequest, ToolExecutionResult};
use crate::workspace::{DiscoveryFilteredWorkspaceBackend, WorkspaceBackend};

use super::request::BatchRequestEntry;
use super::response;

pub(super) fn start_single_async(
    context: &mut ToolContext,
    runner: SubTaskRunner,
    request: SubTaskRequest,
) -> ToolExecutionResult {
    let Some(manager) = context.sub_task_manager.clone() else {
        return response::error_message(
            "Sub-task manager is not available for async mode",
            "sub_task_manager_unavailable",
        );
    };

    let (task_id, session_id) =
        SubTaskManager::next_task_identity(&context.task_id, &request.agent_name);
    let agent_name = request.agent_name.clone();
    let task_description = request.task_description.clone();
    let lineage = lineage_from_request(&request);
    let workspace_backend = manager_workspace_backend(context, &request);
    if let Err(error) = manager.submit_with_context_detailed(
        task_id.clone(),
        session_id.clone(),
        agent_name.clone(),
        task_description.clone(),
        SubTaskSubmissionContext {
            workspace_backend: Some(workspace_backend),
            lineage,
        },
        {
            let assigned_task_id = task_id.clone();
            let assigned_session_id = session_id.clone();
            move || {
                let mut outcome = with_assigned_sub_task_identity(
                    assigned_task_id.clone(),
                    assigned_session_id.clone(),
                    || runner(request),
                );
                outcome.task_id = assigned_task_id;
                outcome.session_id = Some(assigned_session_id);
                outcome
            }
        },
    ) {
        let error_code = error.error_code();
        return response::error_message(error.to_string(), error_code);
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
        return response::error_message(
            "Sub-task manager is not available for async mode",
            "sub_task_manager_unavailable",
        );
    };

    let mut results = Vec::new();
    let mut task_ids = Vec::new();
    let mut started = 0usize;
    let mut failed = 0usize;
    for entry in entries {
        let Some(request) = entry.request else {
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
        let task_title = request.task_description.clone();
        let runner = runner.clone();
        let lineage = lineage_from_request(&request);
        let workspace_backend = manager_workspace_backend(context, &request);
        if let Err(error) = manager.submit_with_context_detailed(
            task_id.clone(),
            session_id.clone(),
            agent_name.to_string(),
            task_title.clone(),
            SubTaskSubmissionContext {
                workspace_backend: Some(workspace_backend),
                lineage,
            },
            {
                let assigned_task_id = task_id.clone();
                let assigned_session_id = session_id.clone();
                move || {
                    let mut outcome = with_assigned_sub_task_identity(
                        assigned_task_id.clone(),
                        assigned_session_id.clone(),
                        || runner(request),
                    );
                    outcome.task_id = assigned_task_id;
                    outcome.session_id = Some(assigned_session_id);
                    outcome
                }
            },
        ) {
            failed += 1;
            results.push(json!({
                "index": entry.index,
                "task_id": task_id,
                "session_id": session_id,
                "agent_name": agent_name,
                "status": "failed",
                "error": error.to_string(),
                "error_code": error.error_code(),
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

    let payload = json!({
        "summary": {
            "total": total,
            "accepted": started,
            "failed": failed,
        },
        "task_ids": task_ids,
        "results": results,
        "wait_for_completion": false,
    });
    if started == 0 {
        return response::all_batch_tasks_failed(payload);
    }
    response::success(payload)
}

fn manager_workspace_backend(
    context: &ToolContext,
    request: &SubTaskRequest,
) -> Arc<dyn WorkspaceBackend> {
    let Some(pattern) = request.exclude_files_pattern.as_deref() else {
        return context.workspace_backend.clone();
    };
    Arc::new(
        DiscoveryFilteredWorkspaceBackend::new(context.workspace_backend.clone(), pattern)
            .expect("exclude_files_pattern was validated before async submission"),
    )
}

fn lineage_from_request(request: &SubTaskRequest) -> SubTaskLineage {
    SubTaskLineage {
        parent_run_id: request
            .metadata
            .get("parent_run_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        parent_tool_call_id: request
            .metadata
            .get("parent_tool_call_id")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}
