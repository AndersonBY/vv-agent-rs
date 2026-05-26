use std::collections::BTreeMap;

use serde_json::{json, Value};

pub(super) fn append_unique_string(state: &mut BTreeMap<String, Value>, key: &str, value: String) {
    let entry = state
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !entry.is_array() {
        *entry = Value::Array(Vec::new());
    }
    if let Some(items) = entry.as_array_mut() {
        if !items.iter().any(|item| item.as_str() == Some(&value)) {
            items.push(Value::String(value));
        }
    }
}

pub(super) fn append_activation_log(
    state: &mut BTreeMap<String, Value>,
    skill_name: String,
    reason: String,
    cycle_index: u32,
) {
    let entry = state
        .entry("skill_activation_log".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !entry.is_array() {
        *entry = Value::Array(Vec::new());
    }
    if let Some(items) = entry.as_array_mut() {
        items.push(json!({
            "skill_name": skill_name,
            "reason": reason,
            "cycle_index": cycle_index,
        }));
    }
}
