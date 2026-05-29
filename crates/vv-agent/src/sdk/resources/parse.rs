use std::collections::BTreeMap;

use serde_json::Value;

use crate::types::{Metadata, NoToolPolicy, SubAgentConfig};

pub(super) fn read_string(payload: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn read_bool(
    payload: &serde_json::Map<String, Value>,
    key: &str,
    default: bool,
) -> bool {
    payload
        .get(key)
        .map(json_value_is_truthy)
        .unwrap_or(default)
}

fn json_value_is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

pub(super) fn read_positive_u32(
    payload: &serde_json::Map<String, Value>,
    key: &str,
    default: u32,
) -> u32 {
    let parsed = payload
        .get(key)
        .and_then(json_int)
        .unwrap_or(i64::from(default));
    u32::try_from(parsed.max(1)).unwrap_or(u32::MAX)
}

pub(super) fn read_positive_u64(
    payload: &serde_json::Map<String, Value>,
    key: &str,
    default: u64,
) -> u64 {
    let parsed = payload
        .get(key)
        .and_then(json_int)
        .unwrap_or_else(|| i64::try_from(default).unwrap_or(i64::MAX));
    u64::try_from(parsed.max(1)).unwrap_or(u64::MAX)
}

pub(super) fn read_percentage_u8(
    payload: &serde_json::Map<String, Value>,
    key: &str,
    default: u8,
) -> u8 {
    let parsed = payload
        .get(key)
        .and_then(json_int)
        .unwrap_or(i64::from(default));
    u8::try_from(parsed.clamp(1, 100)).unwrap_or(default)
}

fn json_int(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(value) => Some(i64::from(*value)),
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| number.as_f64().map(|value| value.trunc() as i64)),
        Value::String(value) => value.trim().parse::<i64>().ok(),
        _ => None,
    }
}

pub(super) fn read_no_tool_policy(payload: &serde_json::Map<String, Value>) -> NoToolPolicy {
    match read_string(payload, "no_tool_policy").as_deref() {
        Some("finish") => NoToolPolicy::Finish,
        Some("wait_user") => NoToolPolicy::WaitUser,
        _ => NoToolPolicy::Continue,
    }
}

pub(super) fn read_string_list(payload: &serde_json::Map<String, Value>, key: &str) -> Vec<String> {
    payload
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(stringify_json_value)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn read_string_map(
    payload: &serde_json::Map<String, Value>,
    key: &str,
) -> BTreeMap<String, String> {
    payload
        .get(key)
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    let key = key.trim();
                    if key.is_empty() {
                        return None;
                    }
                    Some((key.to_string(), stringify_json_value(value)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn stringify_json_value(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(items) => {
            let items = items
                .iter()
                .map(json_value_repr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{items}]")
        }
        Value::Object(object) => {
            let items = object
                .iter()
                .map(|(key, value)| {
                    format!("{}: {}", quote_json_string(key), json_value_repr(value))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{items}}}")
        }
    }
}

fn json_value_repr(value: &Value) -> String {
    match value {
        Value::String(value) => quote_json_string(value),
        other => stringify_json_value(other),
    }
}

fn quote_json_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

pub(super) fn read_metadata(payload: &serde_json::Map<String, Value>, key: &str) -> Metadata {
    payload
        .get(key)
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn read_sub_agents(
    payload: &serde_json::Map<String, Value>,
) -> BTreeMap<String, SubAgentConfig> {
    let mut sub_agents = BTreeMap::new();
    let Some(object) = payload.get("sub_agents").and_then(Value::as_object) else {
        return sub_agents;
    };
    for (name, raw_config) in object {
        let Some(config) = raw_config.as_object() else {
            continue;
        };
        let Some(model) = read_string(config, "model") else {
            continue;
        };
        let Some(description) = read_string(config, "description") else {
            continue;
        };
        let mut sub_agent = SubAgentConfig::new(model, description);
        sub_agent.backend = read_string(config, "backend");
        sub_agent.system_prompt = read_string(config, "system_prompt");
        sub_agent.max_cycles = read_positive_u32(config, "max_cycles", 8);
        sub_agent.exclude_tools = read_string_list(config, "exclude_tools");
        sub_agent.metadata = read_metadata(config, "metadata");
        sub_agents.insert(name.clone(), sub_agent);
    }
    sub_agents
}
