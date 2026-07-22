use crate::types::Metadata;
use serde_json::{Map, Value};

pub(in crate::types::dict) fn expect_object<'a>(
    value: &'a Value,
    type_name: &str,
) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{type_name} payload must be an object"))
}

pub(in crate::types::dict) fn read_required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required string field {key:?}"))
}

pub(in crate::types::dict) fn read_string(
    object: &Map<String, Value>,
    key: &str,
) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

pub(in crate::types::dict) fn read_optional_string(
    object: &Map<String, Value>,
    key: &str,
) -> Option<String> {
    object
        .get(key)
        .filter(|value| !value.is_null())
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(in crate::types::dict) fn read_bool(
    object: &Map<String, Value>,
    key: &str,
    default: bool,
) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(default)
}

pub(in crate::types::dict) fn read_u32(
    object: &Map<String, Value>,
    key: &str,
    default: u32,
) -> u32 {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

pub(in crate::types::dict) fn read_array<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Option<&'a [Value]> {
    object.get(key).and_then(Value::as_array).map(Vec::as_slice)
}

pub(in crate::types::dict) fn read_metadata(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Metadata, String> {
    match object.get(key) {
        Some(Value::Object(map)) => Ok(map.clone().into_iter().collect()),
        Some(Value::Null) | None => Ok(Metadata::new()),
        Some(_) => Err(format!("{key:?} must be an object")),
    }
}
