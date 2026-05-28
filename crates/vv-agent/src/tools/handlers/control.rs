use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{coerce_python_bool_arg, coerce_python_text_arg};
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
                .map(|value| coerce_python_text_arg(Some(value), "").trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "Task completed".to_string());
            let require_all_done = arguments
                .get("require_all_todos_completed")
                .map(|value| coerce_python_bool_arg(Some(value), true))
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
                                let done = coerce_python_bool_arg(todo.get("done"), false);
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
                        let path = coerce_python_text_arg(Some(path), "").trim().to_string();
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
    if let Some(schema) = super::super::schemas::schema_for("task_finish") {
        spec.schema = schema;
    }
    spec
}

pub fn ask_user(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = ask_user_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn ask_user_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "ask_user",
        "Ask the user a question and pause the agent until the user responds.",
        Arc::new(|_context, arguments| {
            let question = arguments
                .get("question")
                .map(|value| coerce_python_text_arg(Some(value), "").trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "Need user input".to_string());
            let selection_type = arguments
                .get("selection_type")
                .map(|value| coerce_python_text_arg(Some(value), "").trim().to_string())
                .filter(|value| value == "single" || value == "multi")
                .unwrap_or_else(|| "single".to_string());
            let allow_custom_options = arguments
                .get("allow_custom_options")
                .map(|value| coerce_python_bool_arg(Some(value), false))
                .unwrap_or(false);
            let mut payload = BTreeMap::new();
            payload.insert("question".to_string(), Value::String(question.clone()));
            payload.insert("selection_type".to_string(), Value::String(selection_type));
            payload.insert(
                "allow_custom_options".to_string(),
                Value::Bool(allow_custom_options),
            );
            if let Some(options) = normalize_ask_user_options(arguments.get("options")) {
                payload.insert("options".to_string(), Value::Array(options));
            }
            ToolExecutionResult {
                tool_call_id: String::new(),
                content: Value::Object(payload.clone().into_iter().collect()).to_string(),
                status: ToolResultStatus::Success,
                directive: ToolDirective::WaitUser,
                error_code: None,
                metadata: payload,
                image_url: None,
                image_path: None,
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("ask_user") {
        spec.schema = schema;
    }
    spec
}

fn normalize_ask_user_options(raw: Option<&Value>) -> Option<Vec<Value>> {
    let options = raw?.as_array()?;
    let mut normalized = Vec::new();
    for option in options {
        let option_text = coerce_python_text_arg(Some(option), "").trim().to_string();
        if option_text.is_empty() {
            continue;
        }
        if normalized
            .iter()
            .any(|seen: &Value| seen.as_str() == Some(option_text.as_str()))
        {
            continue;
        }
        normalized.push(Value::String(option_text));
    }
    (!normalized.is_empty()).then_some(normalized)
}
