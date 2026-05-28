use std::collections::BTreeMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn hash_system_prompt_sections(sections: &[Value]) -> String {
    let normalized = sections
        .iter()
        .filter_map(normalize_section)
        .collect::<Vec<_>>();
    if normalized.is_empty() {
        return String::new();
    }
    hash_json(&normalized)
}

pub fn hash_tool_payload(tools: &[Value]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    hash_json(tools)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheBreakTracker {
    last_system_hash: String,
    last_tool_hash: String,
    total_requests: usize,
    cache_breaks: usize,
    break_reasons: Vec<String>,
}

impl CacheBreakTracker {
    pub fn check(&mut self, system_hash: String, tool_hash: String) -> Vec<String> {
        let mut reasons = Vec::new();
        if !self.last_system_hash.is_empty() && system_hash != self.last_system_hash {
            reasons.push("system_prompt_changed".to_string());
        }
        if !self.last_tool_hash.is_empty() && tool_hash != self.last_tool_hash {
            reasons.push("tool_schemas_changed".to_string());
        }

        self.last_system_hash = system_hash;
        self.last_tool_hash = tool_hash;
        self.total_requests += 1;
        if !reasons.is_empty() {
            self.cache_breaks += 1;
            self.break_reasons.extend(reasons.clone());
        }
        reasons
    }

    pub fn total_requests(&self) -> usize {
        self.total_requests
    }

    pub fn cache_breaks(&self) -> usize {
        self.cache_breaks
    }

    pub fn break_reasons(&self) -> Vec<String> {
        self.break_reasons.clone()
    }

    pub fn cache_hit_rate(&self) -> f64 {
        if self.total_requests == 0 {
            return 1.0;
        }
        1.0 - (self.cache_breaks as f64 / self.total_requests as f64)
    }
}

fn normalize_section(section: &Value) -> Option<BTreeMap<String, Value>> {
    let object = section.as_object()?;
    let text = object
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if text.is_empty() {
        return None;
    }
    Some(BTreeMap::from([
        (
            "id".to_string(),
            Value::String(
                object
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            ),
        ),
        ("text".to_string(), Value::String(text.to_string())),
        (
            "stable".to_string(),
            Value::Bool(
                object
                    .get("stable")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            ),
        ),
    ]))
}

fn hash_json<T: serde::Serialize + ?Sized>(value: &T) -> String {
    let value = serde_json::to_value(value).unwrap_or(Value::Null);
    let payload = python_sorted_json(&value);
    let digest = Sha256::digest(payload.as_bytes());
    hex_lower(&digest)
}

fn python_sorted_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => {
            if *value {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(python_sorted_json)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(object) => {
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|(key, value)| format!(
                        "{}: {}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                        python_sorted_json(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}
