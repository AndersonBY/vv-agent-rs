use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

pub type Metadata = BTreeMap<String, Value>;
pub type ToolArguments = BTreeMap<String, Value>;
pub type ToolSchema = Value;

pub fn json_value_from_serializable<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}
