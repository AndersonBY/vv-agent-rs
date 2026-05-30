use std::collections::BTreeMap;

use serde_json::Value;

use crate::skills::errors::SkillValidationError;
use crate::skills::models::SkillProperties;

use super::value::value_to_string;

pub(crate) fn build_properties(
    metadata: &BTreeMap<String, Value>,
) -> Result<SkillProperties, SkillValidationError> {
    let name = required_string(metadata, "name")?;
    let description = required_string(metadata, "description")?;
    Ok(SkillProperties {
        name,
        description,
        license: optional_string(metadata.get("license")),
        allowed_tools: optional_string(metadata.get("allowed-tools")),
        metadata: metadata
            .get("metadata")
            .and_then(Value::as_object)
            .map(string_map_from_json_object)
            .unwrap_or_default(),
    })
}

fn required_string(
    metadata: &BTreeMap<String, Value>,
    key: &str,
) -> Result<String, SkillValidationError> {
    let Some(value) = metadata.get(key).and_then(Value::as_str).map(str::trim) else {
        return Err(SkillValidationError::new(format!(
            "Field '{key}' must be a non-empty string"
        )));
    };
    if value.is_empty() {
        return Err(SkillValidationError::new(format!(
            "Field '{key}' must be a non-empty string"
        )));
    }
    Ok(value.to_string())
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_map_from_json_object(
    object: &serde_json::Map<String, Value>,
) -> BTreeMap<String, String> {
    object
        .iter()
        .map(|(key, value)| (key.clone(), value_to_string(value)))
        .collect()
}
