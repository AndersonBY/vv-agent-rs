use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};

use crate::tools::base::ToolSpec;
use crate::types::{ToolDirective, ToolExecutionResult, ToolResultStatus};

pub(crate) fn task_finish_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "task_finish",
        "Finish the current task and return the final answer to the user.",
        Arc::new(|context, arguments| {
            let message = arguments
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Task completed")
                .to_string();
            let require_all_done = arguments
                .get("require_all_todos_completed")
                .and_then(Value::as_bool)
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
                                let done =
                                    todo.get("done").and_then(Value::as_bool).unwrap_or(false);
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
                metadata.insert(
                    "exposed_files".to_string(),
                    Value::Array(exposed_files.clone()),
                );
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

pub(crate) fn ask_user_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "ask_user",
        "Ask the user a question and pause the agent until the user responds.",
        Arc::new(|_context, arguments| {
            let question = arguments
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or("Need user input")
                .to_string();
            let selection_type = arguments
                .get("selection_type")
                .and_then(Value::as_str)
                .filter(|value| *value == "single" || *value == "multi")
                .unwrap_or("single")
                .to_string();
            let allow_custom_options = arguments
                .get("allow_custom_options")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut payload = BTreeMap::new();
            payload.insert("question".to_string(), Value::String(question.clone()));
            payload.insert("selection_type".to_string(), Value::String(selection_type));
            payload.insert(
                "allow_custom_options".to_string(),
                Value::Bool(allow_custom_options),
            );
            if let Some(options) = arguments.get("options").and_then(Value::as_array) {
                payload.insert("options".to_string(), Value::Array(options.clone()));
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

pub(crate) fn todo_write_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "todo_write",
        "Replace the current todo list for the task.",
        Arc::new(|context, arguments| {
            let Some(todos) = arguments.get("todos").and_then(Value::as_array) else {
                return todo_write_error("`todos` must be an array", "invalid_todos_payload");
            };

            let mut existing_map = BTreeMap::new();
            if let Some(existing_todos) = context
                .shared_state
                .get("todo_list")
                .and_then(Value::as_array)
            {
                for item in existing_todos {
                    let Some(item_id) = item.get("id").map(todo_value_to_string) else {
                        continue;
                    };
                    if !item_id.is_empty() {
                        existing_map.insert(item_id, item.clone());
                    }
                }
            }

            let now = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true);
            let mut new_todo_list = Vec::new();

            for (index, raw_todo_item) in todos.iter().enumerate() {
                let Some(todo_object) = raw_todo_item.as_object() else {
                    return todo_write_error(
                        format!("TODO item at index {index} must be an object"),
                        "invalid_todo_item",
                    );
                };

                let title = todo_object
                    .get("title")
                    .map(todo_value_to_string)
                    .unwrap_or_default();
                if title.is_empty() {
                    return todo_write_error(
                        format!("TODO item at index {index} is missing `title`"),
                        "todo_title_required",
                    );
                }

                let status = todo_object
                    .get("status")
                    .map(todo_value_to_string)
                    .unwrap_or_else(|| "pending".to_string())
                    .to_ascii_lowercase();
                if !matches!(status.as_str(), "pending" | "in_progress" | "completed") {
                    return todo_write_error(
                        format!("TODO item {title} has invalid status {status}"),
                        "invalid_todo_status",
                    );
                }

                let priority = todo_object
                    .get("priority")
                    .map(todo_value_to_string)
                    .unwrap_or_else(|| "medium".to_string())
                    .to_ascii_lowercase();
                if !matches!(priority.as_str(), "low" | "medium" | "high") {
                    return todo_write_error(
                        format!("TODO item {title} has invalid priority {priority}"),
                        "invalid_todo_priority",
                    );
                }

                let item_id = todo_object
                    .get("id")
                    .map(todo_value_to_string)
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(generate_todo_id);
                let created_at = existing_map
                    .get(&item_id)
                    .and_then(|item| item.get("created_at"))
                    .map(todo_value_to_string)
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| now.clone());

                new_todo_list.push(json!({
                    "id": item_id,
                    "title": title,
                    "status": status,
                    "priority": priority,
                    "created_at": created_at,
                    "updated_at": now.clone(),
                }));
            }

            let in_progress_count = new_todo_list
                .iter()
                .filter(|todo| {
                    todo.get("status")
                        .and_then(Value::as_str)
                        .is_some_and(|status| status == "in_progress")
                })
                .count();
            if in_progress_count > 1 {
                return todo_write_error(
                    "Only one TODO item can be in_progress at a time",
                    "multiple_in_progress_todos",
                );
            }

            context
                .shared_state
                .insert("todo_list".to_string(), Value::Array(new_todo_list.clone()));
            ToolExecutionResult::success(
                "",
                json!({
                    "action": "write",
                    "todos": new_todo_list,
                    "count": new_todo_list.len(),
                    "message": format!("TODO list updated successfully with {} items", new_todo_list.len()),
                })
                .to_string(),
            )
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("todo_write") {
        spec.schema = schema;
    }
    spec
}

fn todo_write_error(message: impl Into<String>, error_code: &str) -> ToolExecutionResult {
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: json!({
            "error": message.into(),
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

fn todo_value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.trim().to_string(),
        Value::Null => String::new(),
        other => other.to_string().trim().to_string(),
    }
}

fn generate_todo_id() -> String {
    static TODO_ID_COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = TODO_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp = Utc::now().timestamp_nanos_opt().unwrap_or_default() as u64;
    format!("{:08x}", (timestamp ^ counter) & 0xffff_ffff)
}
