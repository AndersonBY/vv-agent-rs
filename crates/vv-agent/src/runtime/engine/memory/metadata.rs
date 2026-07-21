use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::Value;

pub(super) fn read_u64_metadata(
    metadata: &BTreeMap<String, Value>,
    key: &str,
    default: u64,
) -> u64 {
    metadata
        .get(key)
        .and_then(|value| match value {
            Value::Number(number) => number.as_u64(),
            Value::String(text) => text.trim().parse::<u64>().ok(),
            _ => None,
        })
        .unwrap_or(default)
}

pub(super) fn read_optional_u64_metadata(
    metadata: &BTreeMap<String, Value>,
    key: &str,
) -> Option<u64> {
    metadata.get(key).and_then(|value| match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    })
}

pub(super) fn read_usize_metadata(
    metadata: &BTreeMap<String, Value>,
    key: &str,
    default: usize,
) -> usize {
    read_u64_metadata(metadata, key, default as u64) as usize
}

pub(super) fn read_f64_metadata(
    metadata: &BTreeMap<String, Value>,
    key: &str,
    default: f64,
    minimum: f64,
    maximum: Option<f64>,
) -> f64 {
    let mut value = metadata
        .get(key)
        .and_then(parse_f64_metadata_value)
        .unwrap_or(default)
        .max(minimum);
    if let Some(maximum) = maximum {
        value = value.min(maximum);
    }
    value
}

pub(super) fn parse_f64_metadata_value(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

pub(super) fn read_bool_metadata(
    metadata: &BTreeMap<String, Value>,
    key: &str,
    default: bool,
) -> bool {
    read_optional_bool_metadata(metadata, key).unwrap_or(default)
}

fn read_optional_bool_metadata(metadata: &BTreeMap<String, Value>, key: &str) -> Option<bool> {
    metadata.get(key).and_then(|value| match value {
        Value::Bool(flag) => Some(*flag),
        Value::Number(number) => match number.as_i64() {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        },
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "y" | "on" => Some(true),
            "false" | "0" | "no" | "n" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

pub(super) fn session_memory_enabled(metadata: &BTreeMap<String, Value>) -> bool {
    read_optional_bool_metadata(metadata, "session_memory_enabled")
        .or_else(|| read_optional_bool_metadata(metadata, "enable_session_memory"))
        .unwrap_or_else(|| !read_bool_metadata(metadata, "is_sub_task", false))
}

pub(super) fn read_string_metadata(
    metadata: &BTreeMap<String, Value>,
    key: &str,
    default: &str,
) -> String {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_string()
}

pub(super) fn read_optional_string_metadata(
    metadata: &BTreeMap<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        metadata
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

pub(super) fn read_string_set_metadata(
    metadata: &BTreeMap<String, Value>,
    key: &str,
) -> Option<BTreeSet<String>> {
    let values = metadata.get(key)?.as_array()?;
    let values = values
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    (!values.is_empty()).then_some(values)
}

pub(super) fn metadata_path(
    metadata: &BTreeMap<String, Value>,
    key: &str,
    default: &str,
) -> PathBuf {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}
