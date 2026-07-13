use std::collections::BTreeMap;

use serde_json::Value;

use crate::runtime::shell::normalize_windows_shell_priority;

pub(super) type ShellDefaults = (
    Option<String>,
    Option<Vec<String>>,
    Option<BTreeMap<String, String>>,
);

pub(super) fn read_shell_defaults(
    metadata: &BTreeMap<String, Value>,
) -> Result<ShellDefaults, String> {
    let shell = normalize_shell_value(metadata.get("bash_shell"))?;
    let windows_shell_priority =
        normalize_windows_shell_priority(metadata.get("windows_shell_priority"))?;
    let bash_env = normalize_bash_env(metadata.get("bash_env"))?;
    Ok((shell, windows_shell_priority, bash_env))
}

fn normalize_shell_value(value: Option<&Value>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(value) = value.as_str() else {
        return Err("`bash_shell` must be a string shell name".to_string());
    };
    let value = value.trim().to_string();
    Ok((!value.is_empty()).then_some(value))
}

fn normalize_bash_env(raw: Option<&Value>) -> Result<Option<BTreeMap<String, String>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Some(object) = raw.as_object() else {
        return Err("`bash_env` must be an object mapping env names to values".to_string());
    };
    let mut normalized = BTreeMap::new();
    for (key, value) in object {
        let env_name = key.trim();
        if env_name.is_empty() {
            return Err("`bash_env` contains empty env variable name".to_string());
        }
        normalized.insert(env_name.to_string(), value_to_string(value));
    }
    Ok(Some(normalized))
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        other => other.to_string(),
    }
}
