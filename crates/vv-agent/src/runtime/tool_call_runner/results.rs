use std::collections::BTreeMap;

use crate::tools::{ToolContext, ToolRegistry};
use crate::types::{Message, ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus};

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
            "error_code": error_code,
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

pub(super) fn image_notification_from_tool_result(
    result: &ToolExecutionResult,
    include_image: bool,
) -> Option<Message> {
    if !include_image {
        return None;
    }
    if let Some(image_url) = &result.image_url {
        let content = result
            .image_path
            .as_deref()
            .map(|image_path| format!("[Image loaded] {image_path}"))
            .unwrap_or_default();
        let mut image_message = Message::user(content);
        image_message.image_url = Some(image_url.clone());
        image_message.metadata = result.metadata.clone();
        return Some(image_message);
    }
    result
        .image_path
        .as_deref()
        .map(|image_path| Message::user(format!("[Image loaded] {image_path}")))
}
