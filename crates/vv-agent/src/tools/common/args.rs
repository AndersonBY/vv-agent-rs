use serde_json::Value;

pub(crate) fn coerce_bool(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => match value.as_i64() {
            Some(0) => false,
            Some(1) => true,
            _ => default,
        },
        Some(Value::String(value)) => match trim_portable_whitespace(value)
            .to_ascii_lowercase()
            .as_str()
        {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        _ => default,
    }
}

pub(crate) fn coerce_truthy_arg(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|value| value != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(object)) => !object.is_empty(),
        None => default,
    }
}

pub(crate) fn parse_integer_arg(value: &Value) -> Result<i64, ()> {
    match value {
        Value::Number(number) => number.as_i64().ok_or(()),
        Value::String(text) => trim_portable_whitespace(text)
            .parse::<i64>()
            .map_err(|_| ()),
        _ => Err(()),
    }
}

pub(crate) fn trim_portable_whitespace(value: &str) -> &str {
    value.trim_matches(|character: char| {
        character.is_whitespace() || matches!(character, '\u{001c}'..='\u{001f}')
    })
}

pub(crate) fn stringify_tool_arg(value: Option<&Value>, default: &str) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(boolean)) => {
            if *boolean {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        Some(Value::Null) => "None".to_string(),
        Some(other) => other.to_string(),
        None => default.to_string(),
    }
}
