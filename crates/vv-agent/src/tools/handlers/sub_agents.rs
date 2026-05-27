use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{coerce_bool, tool_error_with_code, tool_result};
use crate::types::{
    AgentStatus, SubTaskRequest, ToolArguments, ToolDirective, ToolExecutionResult,
    ToolResultStatus,
};

pub fn create_sub_task(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    let spec = create_sub_task_tool();
    (spec.handler)(context, arguments)
}

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
                .is_some_and(|value| coerce_bool(Some(value), false));
            let exclude_files_pattern = arguments
                .get("exclude_files_pattern")
                .and_then(Value::as_str)
                .map(str::to_string);
            let wait_for_completion = arguments
                .get("wait_for_completion")
                .is_none_or(|value| coerce_bool(Some(value), true));

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
                    if let Err(error) = manager.submit_with_workspace(
                        task_id.clone(),
                        session_id.clone(),
                        agent_name.clone(),
                        task_title.clone(),
                        Some(context.workspace_backend.clone()),
                        move || runner(request),
                    ) {
                        results.push(json!({
                            "index": index,
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
            let mut prepared_requests = Vec::new();
            let mut invalid_results = BTreeMap::new();
            for (index, item) in tasks.iter().enumerate() {
                let Some(task_description) = item
                    .get("task_description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    invalid_results.insert(
                        index,
                        json!({
                            "index": index,
                            "status": "failed",
                            "error": "`task_description` is required",
                        }),
                    );
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
                prepared_requests.push((index, request));
            }

            let outcomes = if let Some(backend) = context.execution_backend.clone() {
                let runner = runner.clone();
                backend.parallel_map(
                    move |(index, request)| (index, runner(request)),
                    prepared_requests,
                )
            } else {
                prepared_requests
                    .into_iter()
                    .map(|(index, request)| (index, runner(request)))
                    .collect()
            };
            let outcome_map: BTreeMap<_, _> = outcomes.into_iter().collect();
            let mut results = Vec::new();
            let mut completed = 0usize;
            let mut failed = 0usize;
            for index in 0..tasks.len() {
                if let Some(payload) = invalid_results.remove(&index) {
                    failed += 1;
                    results.push(payload);
                    continue;
                }
                let outcome = outcome_map
                    .get(&index)
                    .expect("valid sub-task request should have an outcome");
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
