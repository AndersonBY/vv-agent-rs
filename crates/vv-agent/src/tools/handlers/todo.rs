use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::stringify_tool_arg;
use crate::types::{ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

pub(crate) fn todo_write_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "todo_write",
        "Replace the current todo list for the task.",
        Arc::new(todo_write),
    );
    if let Some(schema) = super::super::schemas::schema_for("todo_write") {
        spec.schema = schema;
    }
    spec
}

pub fn todo_write(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
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

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false);
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
}

pub fn todo_read(context: &mut ToolContext, _arguments: &ToolArguments) -> ToolExecutionResult {
    let todos = context
        .shared_state
        .entry("todo_list".to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array()
        .cloned()
        .unwrap_or_default();
    ToolExecutionResult::success(
        "",
        json!({
            "action": "read",
            "todos": todos,
            "count": todos.len(),
        })
        .to_string(),
    )
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
    stringify_tool_arg(Some(value), "").trim().to_string()
}

fn generate_todo_id() -> String {
    static TODO_ID_COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = TODO_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp = Utc::now().timestamp_nanos_opt().unwrap_or_default() as u64;
    format!("{:08x}", (timestamp ^ counter) & 0xffff_ffff)
}
