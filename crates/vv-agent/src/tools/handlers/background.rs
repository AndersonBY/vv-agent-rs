use std::sync::Arc;

use serde_json::Value;

use crate::runtime::background_sessions::background_session_manager;
use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{string_arg, tool_error_with_code, tool_result_with_metadata};
use crate::types::{
    Metadata, ToolArguments, ToolArtifactRef, ToolDirective, ToolExecutionResult, ToolResultStatus,
    ToolTruncationReason,
};

pub fn check_background_command(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    let spec = check_background_command_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn check_background_command_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "check_background_command",
        "Check status and output for a background command.",
        Arc::new(|context, arguments| {
            let session_id = string_arg(arguments.get("session_id"), "");
            let session_id = session_id.trim();
            if session_id.is_empty() {
                return tool_error_with_code("`session_id` is required", "session_id_required");
            }
            let payload = background_session_manager().check_for_tool(
                session_id,
                context.effective_workspace_backend(),
                &context.task_id,
                &context.tool_call_id,
            );
            if let Some(error) = payload.get("artifact_error").and_then(Value::as_str) {
                let error_code = payload
                    .get("artifact_error_code")
                    .and_then(Value::as_str)
                    .unwrap_or("artifact_persist_failed");
                return tool_error_with_code(
                    format!("failed to persist complete command output: {error}"),
                    error_code,
                );
            }
            match payload.get("status").and_then(Value::as_str) {
                Some("running") => tool_result_with_metadata(
                    ToolResultStatus::Running,
                    payload.clone(),
                    None,
                    ToolDirective::Continue,
                    background_metadata(&payload),
                ),
                Some("completed") => terminal_background_result(payload, true),
                Some("failed" | "timeout") => terminal_background_result(payload, false),
                _ => background_error(payload),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("check_background_command") {
        spec.schema = schema;
    }
    spec
}

fn terminal_background_result(payload: Value, success: bool) -> ToolExecutionResult {
    let mut content = payload
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if content.is_empty() && !success {
        content = "Background command failed".to_string();
    }
    let mut result = ToolExecutionResult::success("", content);
    result.status = if success {
        ToolResultStatus::Success
    } else {
        ToolResultStatus::Error
    };
    result.error_code = (!success).then(|| "background_command_failed".to_string());
    result.metadata = background_metadata(&payload);
    if payload.get("output_truncated").and_then(Value::as_bool) == Some(true) {
        let artifact = payload
            .get("artifact")
            .cloned()
            .and_then(|value| serde_json::from_value::<ToolArtifactRef>(value).ok());
        let Some(artifact) = artifact else {
            return tool_error_with_code(
                "complete background output has no recoverable artifact",
                "artifact_persist_failed",
            );
        };
        result.truncated = true;
        result.truncation_reason = Some(ToolTruncationReason::OutputLimit);
        result.original_bytes = payload.get("output_original_bytes").and_then(Value::as_u64);
        result.visible_bytes = payload.get("output_visible_bytes").and_then(Value::as_u64);
        result.artifact = Some(artifact);
    }
    result
}

fn background_metadata(payload: &Value) -> Metadata {
    let Some(object) = payload.as_object() else {
        return Metadata::new();
    };
    [
        "status",
        "session_id",
        "elapsed_seconds",
        "exit_code",
        "shell",
    ]
    .into_iter()
    .filter_map(|key| {
        let value = object.get(key)?;
        (!value.is_null()).then(|| (key.to_string(), value.clone()))
    })
    .collect()
}

fn background_error(payload: Value) -> ToolExecutionResult {
    let mut object = payload.as_object().cloned().unwrap_or_default();
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("missing")
        .to_string();
    let error = object
        .remove("error")
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| match status.as_str() {
            "timeout" => "Background command timed out".to_string(),
            _ => "Background command failed".to_string(),
        });
    object.insert("ok".to_string(), Value::Bool(false));
    object.insert("error".to_string(), Value::String(error));
    object.insert(
        "error_code".to_string(),
        Value::String("background_command_failed".to_string()),
    );
    let content = Value::Object(object);
    let metadata = background_metadata(&content);
    tool_result_with_metadata(
        ToolResultStatus::Error,
        content,
        Some("background_command_failed"),
        ToolDirective::Continue,
        metadata,
    )
}
