use std::collections::BTreeMap;

use serde_json::Value;

pub(super) fn truthy_trimmed_string(value: Option<&Value>) -> String {
    truthy_string(value).trim().to_string()
}

fn truthy_string(value: Option<&Value>) -> String {
    let Some(value) = value.filter(|value| json_value_is_truthy(value)) else {
        return String::new();
    };
    stringify_json_value(value)
}

pub(super) fn truthy_value_string(value: &Value) -> String {
    if json_value_is_truthy(value) {
        stringify_json_value(value)
    } else {
        String::new()
    }
}

fn json_value_is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(object) => !object.is_empty(),
    }
}

fn stringify_json_value(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(_) | Value::Object(_) => json_value_repr(value),
    }
}

fn json_value_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(value) => format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Array(items) => {
            let values = items.iter().map(json_value_repr).collect::<Vec<_>>();
            format!("[{}]", values.join(", "))
        }
        Value::Object(object) => {
            let values = object
                .iter()
                .map(|(key, value)| {
                    format!("'{}': {}", key.replace('\'', "\\'"), json_value_repr(value))
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", values.join(", "))
        }
    }
}

pub(super) fn string_map_from_json_object(
    object: &serde_json::Map<String, Value>,
) -> BTreeMap<String, String> {
    object
        .iter()
        .map(|(key, value)| (key.clone(), stringify_json_value(value)))
        .collect()
}
