use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::tools::base::ToolContext;

pub fn to_json(data: &Value) -> String {
    serde_json::to_string(data).unwrap_or_else(|_| "null".to_string())
}

pub fn is_string_keyed_dict(value: &Value) -> bool {
    value.is_object()
}

pub fn get_todo_list(shared_state: &mut BTreeMap<String, Value>) -> &mut Vec<Value> {
    let value = shared_state
        .entry("todo_list".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !value.is_array() {
        *value = Value::Array(Vec::new());
    }
    match value {
        Value::Array(items) => items,
        _ => unreachable!("todo_list was normalized to an array"),
    }
}

pub fn normalize_todo_items(raw_items: &Value) -> Vec<Value> {
    let Some(items) = raw_items.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            let title = object
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if title.is_empty() {
                return None;
            }
            Some(json!({
                "title": title,
                "done": object.get("done").is_some_and(value_truthy),
            }))
        })
        .collect()
}

pub fn resolve_workspace_path(context: &ToolContext, raw_path: &str) -> Result<PathBuf, String> {
    context.resolve_workspace_path(raw_path)
}

fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(object) => !object.is_empty(),
    }
}
