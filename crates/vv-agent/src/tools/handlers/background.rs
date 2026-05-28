use std::sync::Arc;

use serde_json::Value;

use crate::runtime::background_sessions::background_session_manager;
use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{stringify_tool_arg, tool_error_with_code, tool_result};
use crate::types::{ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

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
            let session_id = stringify_tool_arg(arguments.get("session_id"), "");
            let session_id = session_id.trim();
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
                Some("completed") => tool_result(
                    ToolResultStatus::Success,
                    payload,
                    None,
                    ToolDirective::Continue,
                ),
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
