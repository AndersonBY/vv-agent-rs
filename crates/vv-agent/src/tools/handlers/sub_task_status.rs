use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{bool_arg, integer_arg, tool_result, trim_portable_whitespace};
use crate::types::{ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

const DEFAULT_SUB_TASK_WAIT_INTERVAL_SECONDS: i64 = 300;
const MIN_SUB_TASK_WAIT_INTERVAL_SECONDS: i64 = 30;
const MAX_SUB_TASK_WAIT_INTERVAL_SECONDS: i64 = 1800;
const DEFAULT_SUB_TASK_MAX_WAIT_SECONDS: i64 = 3600;
const MIN_SUB_TASK_MAX_WAIT_SECONDS: i64 = 60;
const MAX_SUB_TASK_MAX_WAIT_SECONDS: i64 = 24 * 60 * 60;
const LOCAL_SUB_TASK_WAIT_POLL: Duration = Duration::from_millis(100);
const RUNNING_SUB_TASK_STATUSES: &[&str] = &["pending", "running"];

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
                return status_error(
                    "Sub-task manager is not available for this task",
                    "sub_task_manager_unavailable",
                );
            };
            let Some(raw_task_ids) = arguments.get("task_ids").and_then(Value::as_array) else {
                return status_error("`task_ids` must be a non-empty array", "invalid_task_ids");
            };
            if raw_task_ids.is_empty() {
                return status_error("`task_ids` must be a non-empty array", "invalid_task_ids");
            }
            let mut task_ids = Vec::new();
            for item in raw_task_ids {
                let Value::String(task_id) = item else {
                    return status_error(
                        "`task_ids` must contain only strings",
                        "invalid_task_ids",
                    );
                };
                let task_id = trim_portable_whitespace(task_id);
                if !task_id.is_empty() && !task_ids.iter().any(|known| known == task_id) {
                    task_ids.push(task_id.to_string());
                }
            }
            if task_ids.is_empty() {
                return status_error(
                    "`task_ids` must include at least one valid task id",
                    "invalid_task_ids",
                );
            }
            let detail_level = match arguments.get("detail_level") {
                None => "basic".to_string(),
                Some(Value::String(value)) => {
                    let normalized = trim_portable_whitespace(value).to_ascii_lowercase();
                    if normalized == "basic" || normalized == "snapshot" {
                        normalized
                    } else {
                        "basic".to_string()
                    }
                }
                Some(_) => {
                    return status_error("`detail_level` must be a string", "invalid_detail_level");
                }
            };
            let workspace_file_limit = arguments
                .get("workspace_file_limit")
                .and_then(|value| integer_arg(value).ok())
                .unwrap_or(20)
                .clamp(1, 100) as usize;
            let wait_for_completion = bool_arg(arguments.get("wait_for_completion"), false);
            let check_interval_seconds =
                normalize_wait_interval_seconds(arguments.get("check_interval_seconds"));
            let max_wait_seconds = normalize_max_wait_seconds(arguments.get("max_wait_seconds"));
            let message = match arguments.get("message") {
                None => None,
                Some(Value::String(value)) => {
                    let message = trim_portable_whitespace(value);
                    (!message.is_empty()).then(|| message.to_string())
                }
                Some(_) => {
                    return status_error("`message` must be a string", "invalid_sub_task_message");
                }
            };
            let wait_for_response = bool_arg(arguments.get("wait_for_response"), false);
            let mut interaction = None;
            if let Some(message) = message {
                let target_id = task_ids[0].clone();
                let Some(session_id) = manager.task_session_id(&target_id) else {
                    return task_status_error(
                        format!("Sub-task {target_id} not found."),
                        "sub_task_not_found",
                        &target_id,
                    );
                };
                let previous_status = manager
                    .task_status_label(&target_id)
                    .unwrap_or_else(|| "missing".to_string());
                if manager.is_running(&target_id) {
                    if manager.has_attached_session(&target_id) != Some(true) {
                        return task_status_error(
                            format!("Sub-task {target_id} session is not ready yet."),
                            "sub_task_session_not_ready",
                            &target_id,
                        );
                    }
                    if !crate::steer_sub_agent_session(&session_id, &message) {
                        return task_status_error(
                            format!("Failed to queue message for running sub-task {target_id}."),
                            "sub_task_message_queue_failed",
                            &target_id,
                        );
                    }
                    interaction = Some(json!({
                        "task_id": target_id,
                        "action": "message_queued",
                        "previous_status": previous_status,
                    }));
                } else {
                    if previous_status == "max_cycles" {
                        return task_status_error(
                            format!("Sub-task {target_id} reached max cycles and cannot continue."),
                            "sub_task_max_cycles_reached",
                            &target_id,
                        );
                    }
                    let continuation = match context.sub_task_turn_snapshot.clone() {
                        Some(snapshot) => {
                            manager.continue_task_with_snapshot(&target_id, &message, snapshot)
                        }
                        None => manager.continue_task(&target_id, &message),
                    };
                    if let Err(error) = continuation {
                        return task_status_error(error, "sub_task_continue_failed", &target_id);
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
            let (tasks, running_task_ids, wait_exceeded) = if wait_for_completion {
                wait_for_sub_task_completion(
                    &manager,
                    &task_ids,
                    detail_level.as_str(),
                    workspace_file_limit,
                    max_wait_seconds,
                )
            } else {
                let tasks =
                    manager.status_entries(&task_ids, detail_level.as_str(), workspace_file_limit);
                let running_task_ids = running_task_ids(&tasks);
                (tasks, running_task_ids, false)
            };
            let mut payload = json!({
                "tasks": tasks,
                "detail_level": detail_level,
            });
            add_wait_metadata(
                &mut payload,
                wait_for_completion,
                check_interval_seconds,
                max_wait_seconds,
                running_task_ids,
                wait_exceeded,
            );
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

fn status_error(message: impl Into<String>, error_code: &str) -> ToolExecutionResult {
    tool_result(
        ToolResultStatus::Error,
        json!({
            "ok": false,
            "error": message.into(),
            "error_code": error_code,
        }),
        Some(error_code),
        ToolDirective::Continue,
    )
}

fn task_status_error(
    message: impl Into<String>,
    error_code: &str,
    task_id: &str,
) -> ToolExecutionResult {
    tool_result(
        ToolResultStatus::Error,
        json!({
            "ok": false,
            "error": message.into(),
            "error_code": error_code,
            "details": {"task_id": task_id},
        }),
        Some(error_code),
        ToolDirective::Continue,
    )
}

fn normalize_wait_interval_seconds(value: Option<&Value>) -> i64 {
    let seconds = value
        .and_then(|value| integer_arg(value).ok())
        .unwrap_or(DEFAULT_SUB_TASK_WAIT_INTERVAL_SECONDS);
    seconds.clamp(
        MIN_SUB_TASK_WAIT_INTERVAL_SECONDS,
        MAX_SUB_TASK_WAIT_INTERVAL_SECONDS,
    )
}

fn normalize_max_wait_seconds(value: Option<&Value>) -> i64 {
    let seconds = value
        .and_then(|value| {
            if value.is_null() {
                None
            } else {
                integer_arg(value).ok()
            }
        })
        .unwrap_or(DEFAULT_SUB_TASK_MAX_WAIT_SECONDS);
    seconds.clamp(MIN_SUB_TASK_MAX_WAIT_SECONDS, MAX_SUB_TASK_MAX_WAIT_SECONDS)
}

fn running_task_ids(tasks: &[Value]) -> Vec<String> {
    tasks
        .iter()
        .filter_map(|entry| {
            let status = entry.get("status").and_then(Value::as_str)?;
            if !RUNNING_SUB_TASK_STATUSES.contains(&status) {
                return None;
            }
            entry
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn wait_for_sub_task_completion(
    manager: &crate::runtime::SubTaskManager,
    task_ids: &[String],
    detail_level: &str,
    workspace_file_limit: usize,
    max_wait_seconds: i64,
) -> (Vec<Value>, Vec<String>, bool) {
    let deadline = Instant::now() + Duration::from_secs(max_wait_seconds as u64);
    let mut tasks = manager.status_entries(task_ids, detail_level, workspace_file_limit);
    let mut current_running_task_ids = running_task_ids(&tasks);
    let mut wait_exceeded = false;

    while !current_running_task_ids.is_empty() {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            wait_exceeded = true;
            break;
        };
        let wait_slice = remaining.min(LOCAL_SUB_TASK_WAIT_POLL);
        let mut progressed = false;
        for task_id in current_running_task_ids.clone() {
            if manager.wait(&task_id, Some(wait_slice)) {
                progressed = true;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
        }

        tasks = manager.status_entries(task_ids, detail_level, workspace_file_limit);
        let next_running_task_ids = running_task_ids(&tasks);
        if next_running_task_ids.is_empty() {
            current_running_task_ids.clear();
            break;
        }
        if Instant::now() >= deadline {
            current_running_task_ids = next_running_task_ids;
            wait_exceeded = true;
            break;
        }
        if progressed || next_running_task_ids != current_running_task_ids {
            current_running_task_ids = next_running_task_ids;
            continue;
        }
        current_running_task_ids = next_running_task_ids;
    }

    (tasks, current_running_task_ids, wait_exceeded)
}

fn add_wait_metadata(
    payload: &mut Value,
    wait_for_completion: bool,
    check_interval_seconds: i64,
    max_wait_seconds: i64,
    running_task_ids: Vec<String>,
    wait_exceeded: bool,
) {
    if !wait_for_completion {
        if !running_task_ids.is_empty() {
            payload["running_task_ids"] = json!(running_task_ids);
        }
        payload["suggested_next_check_after_seconds"] = json!(check_interval_seconds);
        return;
    }

    payload["wait_for_completion"] = json!(true);
    payload["wait_exceeded"] = json!(wait_exceeded);
    payload["running_task_ids"] = json!(running_task_ids);
    payload["suggested_next_check_after_seconds"] = json!(check_interval_seconds);
    payload["max_wait_seconds"] = json!(max_wait_seconds);
    if wait_exceeded {
        payload["message"] = json!(
            "Sub-task(s) are still running after the maximum wait. Call sub_task_status again later instead of tight polling."
        );
    }
}
