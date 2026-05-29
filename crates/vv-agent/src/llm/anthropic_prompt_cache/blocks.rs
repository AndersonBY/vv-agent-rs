use serde_json::{json, Map, Value};

use super::cache_control_ephemeral;
use super::estimate::value_to_string;

const THINKING_BLOCK_TYPES: &[&str] = &["thinking", "redacted_thinking"];

pub(super) fn ensure_content_blocks(message: &mut Map<String, Value>) -> Vec<Value> {
    normalized_content_blocks(message.get("content"))
}

pub(super) fn content_blocks(message: &Value) -> Vec<Value> {
    normalized_content_blocks(message.get("content"))
}

pub(super) fn set_cache_control(block: &mut Value) {
    if let Some(object) = block.as_object_mut() {
        object.insert("cache_control".to_string(), cache_control_ephemeral());
    }
}

pub(super) fn block_type(block: &Value) -> String {
    let normalized = value_to_string(block.get("type"))
        .trim()
        .to_ascii_lowercase();
    if normalized.is_empty() {
        "text".to_string()
    } else {
        normalized
    }
}

pub(super) fn is_thinking_block_type(block_type: &str) -> bool {
    THINKING_BLOCK_TYPES.contains(&block_type)
}

fn normalized_content_blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::Object(_) => Some(item.clone()),
                Value::String(text) => Some(json!({"type": "text", "text": text})),
                _ => None,
            })
            .collect(),
        Some(Value::String(text)) => vec![json!({"type": "text", "text": text})],
        _ => Vec::new(),
    }
}
