use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::base::ToolContext;
use super::registry::ToolRegistry;
use crate::types::{ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus};

pub fn dispatch_tool_call(
    registry: &ToolRegistry,
    context: &mut ToolContext,
    call: &ToolCall,
) -> ToolExecutionResult {
    if let Some(error) = argument_error_result(call) {
        return error;
    }

    match registry.execute(call, context) {
        Ok(result) => normalize_result(call, result),
        Err(_) => error_result(
            &call.id,
            format!("Unknown tool: {}", call.name),
            Some("tool_not_found"),
        ),
    }
}

pub(crate) fn argument_error_result(call: &ToolCall) -> Option<ToolExecutionResult> {
    let extra = call.extra_content.as_ref()?.as_object()?;
    let error_code = extra
        .get("argument_error_code")
        .and_then(Value::as_str)
        .unwrap_or("invalid_arguments");
    let error = extra
        .get("argument_error")
        .and_then(Value::as_str)
        .unwrap_or("Invalid tool arguments");
    Some(error_result(&call.id, error, Some(error_code)))
}

fn normalize_result(call: &ToolCall, mut result: ToolExecutionResult) -> ToolExecutionResult {
    if needs_tool_call_id(&result.tool_call_id) {
        result.tool_call_id = call.id.clone();
    }
    if result.directive == ToolDirective::WaitUser && result.status == ToolResultStatus::Success {
        result.status = ToolResultStatus::WaitResponse;
    }
    result
}

fn needs_tool_call_id(value: &str) -> bool {
    let stripped = value.trim();
    stripped.is_empty() || stripped == "pending"
}

fn error_result(
    tool_call_id: &str,
    message: impl Into<String>,
    error_code: Option<&str>,
) -> ToolExecutionResult {
    let error_code = error_code.map(str::to_string);
    ToolExecutionResult {
        tool_call_id: tool_call_id.to_string(),
        content: json!({
            "ok": false,
            "error": message.into(),
            "error_code": error_code,
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code,
        metadata: BTreeMap::new(),
        image_url: None,
        image_path: None,
    }
}
