use serde_json::Value;

use super::blocks::{block_type, is_thinking_block_type};

pub(super) fn estimate_tokens(char_count: usize) -> usize {
    if char_count == 0 {
        0
    } else {
        char_count.div_ceil(4)
    }
}

pub(super) fn estimate_tool_chars(tool: &Value) -> usize {
    sorted_json(tool).chars().count()
}

pub(super) fn estimate_block_chars(block: &Value) -> usize {
    let block_type = block_type(block);
    match block_type.as_str() {
        "text" => value_to_string(block.get("text")).chars().count(),
        "tool_result" => json_string(block.get("content").unwrap_or(&Value::Null))
            .chars()
            .count(),
        "tool_use" => {
            value_to_string(block.get("name")).chars().count()
                + json_string(block.get("input").unwrap_or(&Value::Null))
                    .chars()
                    .count()
        }
        candidate if is_thinking_block_type(candidate) => 0,
        _ => json_string(block).chars().count(),
    }
}

pub(super) fn value_to_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(value @ (Value::Array(_) | Value::Object(_))) => json_string(value),
        Some(Value::Null) | None => String::new(),
    }
}

pub(super) fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(object) => !object.is_empty(),
    }
}

fn json_string(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn sorted_json(value: &Value) -> String {
    serde_json::to_string(&sort_value(value)).unwrap_or_default()
}

fn sort_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sort_value).collect()),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), sort_value(value)))
                .collect(),
        ),
        value => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{estimate_block_chars, estimate_tool_chars};

    #[test]
    fn cache_size_estimation_uses_compact_json_and_unicode_characters() {
        let tool = json!({"name": "读取", "input_schema": {"type": "object", "a": 1}});
        assert_eq!(
            estimate_tool_chars(&tool),
            serde_json::to_string(&tool)
                .expect("serialize tool")
                .chars()
                .count()
        );

        let block = json!({"type": "tool_result", "content": {"文本": "你好"}});
        assert_eq!(
            estimate_block_chars(&block),
            serde_json::to_string(&block["content"])
                .expect("serialize content")
                .chars()
                .count()
        );
    }
}
