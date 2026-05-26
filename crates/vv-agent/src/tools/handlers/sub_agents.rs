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

            if !task_description.is_empty() {
                let request = SubTaskRequest {
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
        Arc::new(|_context, _arguments| {
            tool_error_with_code(
                "Sub-task manager is not available for this task",
                "sub_task_manager_unavailable",
            )
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("sub_task_status") {
        spec.schema = schema;
    }
    spec
}
