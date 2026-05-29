use std::collections::BTreeMap;

use serde_json::Value;

use crate::types::{ToolDirective, ToolExecutionResult, ToolResultStatus};

pub(super) fn grep_error(message: impl Into<String>) -> ToolExecutionResult {
    let message = message.into();
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: message.clone(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: None,
        metadata: BTreeMap::from([("error".to_string(), Value::String(message))]),
        image_url: None,
        image_path: None,
    }
}
