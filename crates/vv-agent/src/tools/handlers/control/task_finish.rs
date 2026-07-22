use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{bool_arg, string_arg};
use crate::types::{ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

pub fn task_finish(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = task_finish_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn task_finish_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "task_finish",
        "Finish the current task and return the final answer to the user.",
        Arc::new(|context, arguments| {
            let message = arguments
                .get("message")
                .map(|value| string_arg(Some(value), "").trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "Task completed".to_string());
            let require_all_done = arguments
                .get("require_all_todos_completed")
                .map(|value| bool_arg(Some(value), true))
                .unwrap_or(true);
            if require_all_done {
                let incomplete_todos = context
                    .shared_state
                    .get("todo_list")
                    .and_then(Value::as_array)
                    .map(|todos| {
                        todos
                            .iter()
                            .filter_map(|todo| {
                                let status = todo
                                    .get("status")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_ascii_lowercase();
                                let done = bool_arg(todo.get("done"), false);
                                if matches!(status.as_str(), "completed" | "done" | "finished")
                                    || done
                                {
                                    None
                                } else {
                                    Some(
                                        todo.get("title")
                                            .and_then(Value::as_str)
                                            .unwrap_or("Untitled TODO")
                                            .to_string(),
                                    )
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if !incomplete_todos.is_empty() {
                    return ToolExecutionResult {
                        tool_call_id: String::new(),
                        content: json!({
                            "ok": false,
                            "error_code": "todo_incomplete",
                            "error": "Cannot finish task while todo items are incomplete",
                            "incomplete_todos": incomplete_todos,
                        })
                        .to_string(),
                        status: ToolResultStatus::Error,
                        directive: ToolDirective::Continue,
                        error_code: Some("todo_incomplete".to_string()),
                        metadata: BTreeMap::new(),
                        image_url: None,
                        image_path: None,
                    };
                }
            }
            let mut metadata = BTreeMap::new();
            metadata.insert("final_message".to_string(), Value::String(message.clone()));
            if let Some(exposed_files) = arguments.get("exposed_files").and_then(Value::as_array) {
                let exposed_files = exposed_files
                    .iter()
                    .filter_map(|path| {
                        let path = string_arg(Some(path), "").trim().to_string();
                        (!path.is_empty()).then_some(Value::String(path))
                    })
                    .collect::<Vec<_>>();
                metadata.insert("exposed_files".to_string(), Value::Array(exposed_files));
            }
            ToolExecutionResult {
                tool_call_id: String::new(),
                content: json!({"ok": true, "message": message}).to_string(),
                status: ToolResultStatus::Success,
                directive: ToolDirective::Finish,
                error_code: None,
                metadata,
                image_url: None,
                image_path: None,
            }
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("task_finish") {
        spec.schema = schema;
    }
    spec
}
