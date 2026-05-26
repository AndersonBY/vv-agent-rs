use std::sync::Arc;

use serde_json::Value;

use crate::background_sessions::background_session_manager;
use crate::tools::base::ToolSpec;
use crate::tools::common::{tool_error_with_code, tool_result};
use crate::types::{ToolDirective, ToolExecutionResult, ToolResultStatus};

pub(crate) fn check_background_command_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "check_background_command",
        "Check status and output for a background command.",
        Arc::new(|_context, arguments| {
            let session_id = arguments
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if session_id.is_empty() {
                return tool_error_with_code("`session_id` is required", "session_id_required");
            }
            let payload = background_session_manager().check(session_id);
            match payload.get("status").and_then(Value::as_str) {
                Some("running") => tool_result(
                    ToolResultStatus::Running,
                    payload,
                    None,
                    ToolDirective::Continue,
                ),
                Some("completed") => ToolExecutionResult::success("", payload.to_string()),
                _ => tool_result(
                    ToolResultStatus::Error,
                    payload,
                    Some("background_command_failed"),
                    ToolDirective::Continue,
                ),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("check_background_command") {
        spec.schema = schema;
    }
    spec
}
