use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::ToolSpec;
use crate::tools::common::{tool_error_with_code, tool_result};
use crate::types::{AgentStatus, SubTaskRequest, ToolDirective, ToolResultStatus};

pub(crate) fn create_sub_task_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "create_sub_task",
        "Create sub-tasks for a configured sub-agent.",
        Arc::new(|context, arguments| {
            let Some(runner) = context.sub_task_runner.clone() else {
                return tool_error_with_code(
                    "Sub-agent runtime is not available for this task",
                    "sub_agents_not_enabled",
                );
            };

            let agent_name = arguments
                .get("agent_id")
                .or_else(|| arguments.get("agent_name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if agent_name.is_empty() {
                return tool_error_with_code("`agent_id` is required", "agent_id_required");
            }

            let task_description = arguments
                .get("task_description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let raw_tasks = arguments.get("tasks").and_then(Value::as_array);
            if !task_description.is_empty() && raw_tasks.is_some() {
                return tool_error_with_code(
                    "`task_description` and `tasks` are mutually exclusive",
                    "sub_task_payload_conflict",
                );
            }
            if task_description.is_empty() && raw_tasks.is_none() {
                return tool_error_with_code(
                    "Provide either `task_description` or `tasks`",
                    "sub_task_payload_missing",
                );
            }

            let include_main_summary = arguments
                .get("include_main_summary")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let exclude_files_pattern = arguments
                .get("exclude_files_pattern")
                .and_then(Value::as_str)
                .map(str::to_string);
            let wait_for_completion = arguments
                .get("wait_for_completion")
                .and_then(Value::as_bool)
                .unwrap_or(true);

            if !task_description.is_empty() {
                let mut request = SubTaskRequest {
                    agent_name,
                    task_description,
                    output_requirements: arguments
                        .get("output_requirements")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    include_main_summary,
                    exclude_files_pattern,
                    metadata: BTreeMap::new(),
                };
                if !wait_for_completion {
                    let Some(manager) = context.sub_task_manager.clone() else {
                        return tool_error_with_code(
                            "Sub-task manager is not available for async mode",
                            "sub_task_manager_unavailable",
                        );
                    };
                    let (task_id, session_id) =
                        crate::sub_task_manager::SubTaskManager::next_task_identity(
                            &context.task_id,
                            &request.agent_name,
                        );
                    request
                        .metadata
                        .insert("task_id".to_string(), Value::String(task_id.clone()));
                    request
                        .metadata
                        .insert("session_id".to_string(), Value::String(session_id.clone()));
                    let agent_name = request.agent_name.clone();
                    let task_description = request.task_description.clone();
                    manager.submit(
                        task_id.clone(),
                        session_id.clone(),
                        agent_name.clone(),
                        task_description.clone(),
                        move || runner(request),
                    );
                    return tool_result(
                        ToolResultStatus::Success,
                        json!({
                            "task_id": task_id,
                            "session_id": session_id,
                            "agent_name": agent_name,
                            "status": "running",
                            "task_description": task_description,
                            "wait_for_completion": false,
                        }),
                        None,
                        ToolDirective::Continue,
                    );
                }
                let outcome = runner(request);
                let payload = outcome.to_value();
                if outcome.status == AgentStatus::Completed {
                    return tool_result(
                        ToolResultStatus::Success,
                        payload,
                        None,
                        ToolDirective::Continue,
                    );
                }
                let error_code = if outcome.status == AgentStatus::WaitUser {
                    "sub_task_wait_user"
                } else {
                    "sub_task_failed"
                };
                return tool_result(
                    ToolResultStatus::Error,
                    payload,
                    Some(error_code),
                    ToolDirective::Continue,
                );
            }

            let tasks = raw_tasks.expect("tasks checked");
            if tasks.is_empty() {
                return tool_error_with_code(
                    "`tasks` must be a non-empty array",
                    "invalid_tasks_payload",
                );
            }
            if !wait_for_completion {
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
                for (index, item) in tasks.iter().enumerate() {
                    let Some(task_description) = item
                        .get("task_description")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    else {
                        failed += 1;
                        results.push(json!({
                            "index": index,
                            "status": "failed",
                            "error": "`task_description` is required",
                        }));
                        continue;
                    };
                    let mut request = SubTaskRequest {
                        agent_name: agent_name.clone(),
                        task_description: task_description.to_string(),
                        output_requirements: item
                            .get("output_requirements")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .trim()
                            .to_string(),
                        include_main_summary,
                        exclude_files_pattern: exclude_files_pattern.clone(),
                        metadata: BTreeMap::from([(
                            "batch_index".to_string(),
                            Value::Number((index as u64).into()),
                        )]),
                    };
                    let (task_id, session_id) =
                        crate::sub_task_manager::SubTaskManager::next_task_identity(
                            &context.task_id,
                            &agent_name,
                        );
                    request
                        .metadata
                        .insert("task_id".to_string(), Value::String(task_id.clone()));
                    request
                        .metadata
                        .insert("session_id".to_string(), Value::String(session_id.clone()));
                    let task_title = request.task_description.clone();
                    let runner = runner.clone();
                    manager.submit(
                        task_id.clone(),
                        session_id.clone(),
                        agent_name.clone(),
                        task_title.clone(),
                        move || runner(request),
                    );
                    started += 1;
                    task_ids.push(task_id.clone());
                    results.push(json!({
                        "index": index,
                        "task_id": task_id,
                        "session_id": session_id,
                        "agent_name": agent_name,
                        "status": "running",
                        "task_description": task_title,
                    }));
                }
                return tool_result(
                    ToolResultStatus::Success,
                    json!({
                        "summary": {
                            "total": tasks.len(),
                            "accepted": started,
                            "failed": failed,
                        },
                        "task_ids": task_ids,
                        "results": results,
                        "wait_for_completion": false,
                    }),
                    None,
                    ToolDirective::Continue,
                );
            }
            let mut results = Vec::new();
            let mut completed = 0usize;
            let mut failed = 0usize;
            for (index, item) in tasks.iter().enumerate() {
                let Some(task_description) = item
                    .get("task_description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    failed += 1;
                    results.push(json!({
                        "index": index,
                        "status": "failed",
                        "error": "`task_description` is required",
                    }));
                    continue;
                };
                let request = SubTaskRequest {
                    agent_name: agent_name.clone(),
                    task_description: task_description.to_string(),
                    output_requirements: item
                        .get("output_requirements")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    include_main_summary,
                    exclude_files_pattern: exclude_files_pattern.clone(),
                    metadata: BTreeMap::from([(
                        "batch_index".to_string(),
                        Value::Number((index as u64).into()),
                    )]),
                };
                let outcome = runner(request);
                if outcome.status == AgentStatus::Completed {
                    completed += 1;
                } else {
                    failed += 1;
                }
                let mut payload = outcome.to_value();
                payload["index"] = Value::Number((index as u64).into());
                results.push(payload);
            }

            let payload = json!({
                "summary": {
                    "total": tasks.len(),
                    "completed": completed,
                    "failed": failed,
                },
                "results": results,
                "wait_for_completion": true,
            });
            if completed == 0 {
                return tool_result(
                    ToolResultStatus::Error,
                    payload,
                    Some("create_sub_task_batch_failed"),
                    ToolDirective::Continue,
                );
            }
            tool_result(
                ToolResultStatus::Success,
                payload,
                None,
                ToolDirective::Continue,
            )
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("create_sub_task") {
        spec.schema = schema;
    }
    spec
}

pub(crate) fn sub_task_status_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "sub_task_status",
        "Inspect status for background sub-tasks.",
        Arc::new(|context, arguments| {
            let Some(manager) = context.sub_task_manager.clone() else {
                return tool_error_with_code(
                    "Sub-task manager is not available for this task",
                    "sub_task_manager_unavailable",
                );
            };
            let Some(raw_task_ids) = arguments.get("task_ids").and_then(Value::as_array) else {
                return tool_error_with_code(
                    "`task_ids` must be a non-empty array",
                    "invalid_task_ids",
                );
            };
            let mut task_ids = Vec::new();
            for item in raw_task_ids {
                let task_id = item.as_str().unwrap_or_default().trim();
                if !task_id.is_empty() && !task_ids.iter().any(|known| known == task_id) {
                    task_ids.push(task_id.to_string());
                }
            }
            if task_ids.is_empty() {
                return tool_error_with_code(
                    "`task_ids` must include at least one valid task id",
                    "invalid_task_ids",
                );
            }
            let detail_level = arguments
                .get("detail_level")
                .and_then(Value::as_str)
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| value == "basic" || value == "snapshot")
                .unwrap_or_else(|| "basic".to_string());
            let workspace_file_limit = arguments
                .get("workspace_file_limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 100) as usize;
            let message = arguments
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let mut interaction = None;
            if let Some(message) = message {
                let target_id = task_ids[0].clone();
                let Some(session_id) = manager.task_session_id(&target_id) else {
                    return tool_error_with_code(
                        format!("Sub-task {target_id} not found."),
                        "sub_task_not_found",
                    );
                };
                let previous_status = manager
                    .task_status_label(&target_id)
                    .unwrap_or_else(|| "missing".to_string());
                if manager.is_running(&target_id) {
                    if !crate::steer_sub_agent_session(&session_id, &message) {
                        return tool_error_with_code(
                            format!("Failed to queue message for running sub-task {target_id}."),
                            "sub_task_message_queue_failed",
                        );
                    }
                    interaction = Some(json!({
                        "task_id": target_id,
                        "action": "message_queued",
                        "previous_status": previous_status,
                    }));
                } else {
                    return tool_error_with_code(
                        format!("Sub-task {target_id} is not running and cannot be steered yet."),
                        "sub_task_continue_not_supported",
                    );
                }
            }
            let tasks =
                manager.status_entries(&task_ids, detail_level.as_str(), workspace_file_limit);
            let mut payload = json!({
                "tasks": tasks,
                "detail_level": detail_level,
            });
            if let Some(interaction) = interaction {
                payload["interaction"] = interaction;
            }
            tool_result(
                ToolResultStatus::Success,
                payload,
                None,
                ToolDirective::Continue,
            )
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("sub_task_status") {
        spec.schema = schema;
    }
    spec
}
