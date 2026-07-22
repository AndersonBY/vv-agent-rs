use std::sync::Arc;

use serde_json::Value;

use crate::runtime::background_sessions::background_session_manager;
use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{string_arg, tool_error_with_code, tool_result_with_metadata};
use crate::types::{Metadata, ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

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
        Arc::new(|_context, arguments| {
            let session_id = string_arg(arguments.get("session_id"), "");
            let session_id = session_id.trim();
            if session_id.is_empty() {
                return tool_error_with_code("`session_id` is required", "session_id_required");
            }
            let payload = background_session_manager().check(session_id);
            match payload.get("status").and_then(Value::as_str) {
                Some("running") => tool_result_with_metadata(
                    ToolResultStatus::Running,
                    payload.clone(),
                    None,
                    ToolDirective::Continue,
                    background_metadata(&payload),
                ),
                Some("completed") => tool_result_with_metadata(
                    ToolResultStatus::Success,
                    payload.clone(),
                    None,
                    ToolDirective::Continue,
                    background_metadata(&payload),
                ),
                _ => background_error(payload),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("check_background_command") {
        spec.schema = schema;
    }
    spec
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
