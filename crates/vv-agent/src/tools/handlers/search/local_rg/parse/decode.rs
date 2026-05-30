use base64::Engine as _;
use serde_json::Value;

pub(super) fn decode_rg_field(field: Option<&Value>) -> String {
    let Some(field) = field.and_then(Value::as_object) else {
        return String::new();
    };
    if let Some(text) = field.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    let Some(raw) = field.get("bytes").and_then(Value::as_str) else {
        return String::new();
    };
    base64::engine::general_purpose::STANDARD
        .decode(raw)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default()
}

pub(super) fn if_empty(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

pub(super) fn substring_by_byte_range(text: &str, start: usize, end: usize) -> Option<String> {
    if start > end
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return None;
    }
    Some(text[start..end].to_string())
}
