use serde_json::{json, Value};

use crate::tools::common::tool_result;
use crate::types::{
    AgentStatus, SubTaskOutcome, ToolDirective, ToolExecutionResult, ToolResultStatus,
};

use super::request::BatchRequestEntry;

pub(super) fn format_single_sync_result(outcome: SubTaskOutcome) -> ToolExecutionResult {
    let payload = outcome.to_value();
    if outcome.status == AgentStatus::Completed {
        return success(payload);
    }
    let error_code = if outcome.status == AgentStatus::WaitUser {
        "sub_task_wait_user"
    } else {
        "sub_task_failed"
    };
    error(payload, error_code)
}

pub(super) fn invalid_batch_payload(
    total: usize,
    entries: &[BatchRequestEntry],
    wait_for_completion: bool,
) -> ToolExecutionResult {
    let results = entries
        .iter()
        .map(|entry| {
            json!({
                "index": entry.index,
                "status": "failed",
                "error": entry.error.as_deref().unwrap_or("Invalid task item"),
            })
        })
        .collect::<Vec<_>>();

    error(
        json!({
            "ok": false,
            "error": "No valid sub-tasks were provided",
            "error_code": "invalid_tasks_payload",
            "details": {
                "summary": {
                    "total": total,
                    "accepted": 0,
                    "failed": total,
                },
                "results": results,
                "task_ids": [],
                "wait_for_completion": wait_for_completion,
            },
        }),
        "invalid_tasks_payload",
    )
}

pub(super) fn all_batch_tasks_failed(payload: Value) -> ToolExecutionResult {
    error(
        json!({
            "ok": false,
            "error": "All batch sub-tasks failed",
            "error_code": "create_sub_task_batch_failed",
            "details": payload,
        }),
        "create_sub_task_batch_failed",
    )
}

pub(super) fn success(payload: Value) -> ToolExecutionResult {
    tool_result(
        ToolResultStatus::Success,
        payload,
        None,
        ToolDirective::Continue,
    )
}

pub(super) fn error(payload: Value, error_code: &'static str) -> ToolExecutionResult {
    tool_result(
        ToolResultStatus::Error,
        payload,
        Some(error_code),
        ToolDirective::Continue,
    )
}
