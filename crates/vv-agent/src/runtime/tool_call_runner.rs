use std::collections::BTreeMap;

use crate::tools::{ToolContext, ToolRegistry};
use crate::types::{ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus};

pub(crate) fn execute_tool_result(
    registry: &ToolRegistry,
    call: &ToolCall,
    context: &mut ToolContext,
) -> ToolExecutionResult {
    crate::tools::dispatch_tool_call(registry, context, call)
}

pub(crate) fn needs_tool_call_id(value: &str) -> bool {
    let stripped = value.trim();
    stripped.is_empty() || stripped == "pending"
}

pub(crate) fn skipped_tool_result(
    call: &ToolCall,
    error_code: &str,
    message: &str,
) -> ToolExecutionResult {
    ToolExecutionResult {
        tool_call_id: call.id.clone(),
        content: serde_json::json!({
            "ok": false,
            "error": message,
            "skipped_tool": call.name,
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: Some(error_code.to_string()),
        metadata: BTreeMap::new(),
        image_url: None,
        image_path: None,
    }
}
