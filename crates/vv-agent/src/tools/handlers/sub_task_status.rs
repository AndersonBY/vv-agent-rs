use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{
    coerce_bool, coerce_python_text_arg, parse_integer_arg, tool_error_with_code, tool_result,
};
use crate::types::{ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

pub fn sub_task_status(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    let spec = sub_task_status_tool();
    (spec.handler)(context, arguments)
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
                let task_id = coerce_python_text_arg(Some(item), "");
                let task_id = task_id.trim();
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
                .and_then(|value| parse_integer_arg(value).ok())
                .unwrap_or(20)
                .clamp(1, 100) as usize;
            let message = arguments.get("message").and_then(|value| {
                if value.is_null() {
                    return None;
                }
                let message = coerce_python_text_arg(Some(value), "");
                let message = message.trim();
                (!message.is_empty()).then(|| message.to_string())
            });
            let wait_for_response = coerce_bool(arguments.get("wait_for_response"), false);
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
                    if previous_status == "max_cycles" {
                        return tool_error_with_code(
                            format!("Sub-task {target_id} reached max cycles and cannot continue."),
                            "sub_task_max_cycles_reached",
                        );
                    }
                    if let Err(error) = manager.continue_task(&target_id, &message) {
                        return tool_error_with_code(error, "sub_task_continue_failed");
                    }
                    interaction = Some(json!({
                        "task_id": target_id,
                        "action": "continued",
                        "previous_status": previous_status,
                    }));
                }
                if wait_for_response {
                    manager.wait(&target_id, None);
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
