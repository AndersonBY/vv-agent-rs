use crate::types::Metadata;
use serde_json::{Map, Value};

pub(in crate::types::dict) fn insert_optional_string(
    object: &mut Map<String, Value>,
    key: &str,
    value: &Option<String>,
) {
    if let Some(value) = value {
        object.insert(key.to_string(), Value::String(value.clone()));
    }
}

pub(in crate::types::dict) fn insert_non_empty_optional_string(
    object: &mut Map<String, Value>,
    key: &str,
    value: &Option<String>,
) {
    if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
        object.insert(key.to_string(), Value::String(value.to_string()));
    }
}

pub(in crate::types::dict) fn metadata_to_value(metadata: &Metadata) -> Value {
    Value::Object(metadata.clone().into_iter().collect())
}

pub(in crate::types::dict) fn string_vec_to_value(items: &[String]) -> Value {
    Value::Array(items.iter().cloned().map(Value::String).collect())
}
