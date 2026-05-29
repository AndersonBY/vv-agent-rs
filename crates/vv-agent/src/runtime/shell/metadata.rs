pub fn normalize_windows_shell_priority(
    raw: Option<&serde_json::Value>,
) -> Result<Option<Vec<String>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Some(items) = raw.as_array() else {
        return Err("`windows_shell_priority` must be a list of shell names".to_string());
    };
    let mut normalized = Vec::new();
    for item in items {
        if json_value_is_falsey(item) {
            continue;
        }
        let value = stringify_json_value(item);
        let value = value.trim();
        if value.is_empty() || normalized.iter().any(|seen| seen == value) {
            continue;
        }
        normalized.push(value.to_string());
    }
    Ok(Some(normalized))
}

fn json_value_is_falsey(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Bool(value) => !*value,
        serde_json::Value::Number(number) => number.as_f64() == Some(0.0),
        serde_json::Value::String(value) => value.is_empty(),
        serde_json::Value::Array(items) => items.is_empty(),
        serde_json::Value::Object(object) => object.is_empty(),
    }
}

fn stringify_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(true) => "True".to_string(),
        serde_json::Value::Bool(false) => "False".to_string(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Array(items) => {
            let items = items
                .iter()
                .map(json_value_repr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{items}]")
        }
        serde_json::Value::Object(object) => {
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

fn json_value_repr(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => quote_json_string(value),
        other => stringify_json_value(other),
    }
}

fn quote_json_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}
