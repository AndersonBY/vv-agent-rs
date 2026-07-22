use serde_json::Value;

pub(crate) fn bool_arg(value: Option<&Value>, default: bool) -> bool {
    value.and_then(Value::as_bool).unwrap_or(default)
}

pub(crate) fn integer_arg(value: &Value) -> Result<i64, ()> {
    value.as_i64().ok_or(())
}

pub(crate) fn trim_portable_whitespace(value: &str) -> &str {
    value.trim_matches(|character: char| {
        character.is_whitespace() || matches!(character, '\u{001c}'..='\u{001f}')
    })
}

pub(crate) fn string_arg(value: Option<&Value>, default: &str) -> String {
    value.and_then(Value::as_str).unwrap_or(default).to_string()
}
