use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::types::{ToolDirective, ToolExecutionResult, ToolResultStatus};

pub(crate) fn tool_error(message: impl Into<String>) -> ToolExecutionResult {
    tool_error_with_code(message, "")
}

pub(crate) fn tool_result(
    status: ToolResultStatus,
    content: Value,
    error_code: Option<&str>,
    directive: ToolDirective,
) -> ToolExecutionResult {
    let metadata = content
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: content.to_string(),
        status,
        directive,
        error_code: error_code.map(str::to_string),
        metadata,
        image_url: None,
        image_path: None,
    }
}

pub(crate) fn tool_error_with_code(
    message: impl Into<String>,
    error_code: impl Into<String>,
) -> ToolExecutionResult {
    let error_code = error_code.into();
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: json!({"ok": false, "error": message.into(), "error_code": error_code})
            .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: if error_code.is_empty() {
            None
        } else {
            Some(error_code)
        },
        metadata: BTreeMap::new(),
        image_url: None,
        image_path: None,
    }
}

pub(crate) fn path_escapes_workspace_error(message: impl Into<String>) -> ToolExecutionResult {
    tool_error_with_code(message, "path_escapes_workspace")
}
