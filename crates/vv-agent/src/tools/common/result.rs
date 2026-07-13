use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::types::{Metadata, ToolDirective, ToolExecutionResult, ToolResultStatus};

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
    tool_result_with_metadata(status, content, error_code, directive, metadata)
}

pub(crate) fn tool_result_with_metadata(
    status: ToolResultStatus,
    content: Value,
    error_code: Option<&str>,
    directive: ToolDirective,
    mut metadata: Metadata,
) -> ToolExecutionResult {
    if let Some(error_code) = error_code {
        metadata
            .entry("error_code".to_string())
            .or_insert_with(|| Value::String(error_code.to_string()));
    }
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
    let error_code_option = (!error_code.is_empty()).then_some(error_code.as_str());
    tool_result_with_metadata(
        ToolResultStatus::Error,
        json!({"ok": false, "error": message.into(), "error_code": error_code}),
        error_code_option,
        ToolDirective::Continue,
        Metadata::new(),
    )
}

pub(crate) fn path_escapes_workspace_error(message: impl Into<String>) -> ToolExecutionResult {
    tool_error_with_code(message, "path_escapes_workspace")
}
