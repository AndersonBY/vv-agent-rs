use serde_json::Value;

use super::estimate::{value_to_string, value_truthy};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SystemPromptSection {
    pub(super) text: String,
    pub(super) stable: bool,
}

pub(super) fn normalize_system_prompt_sections(raw: Option<&Value>) -> Vec<SystemPromptSection> {
    let Some(items) = raw.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            let text = value_to_string(object.get("text")).trim().to_string();
            if text.is_empty() {
                return None;
            }
            Some(SystemPromptSection {
                text,
                stable: object.get("stable").is_none_or(value_truthy),
            })
        })
        .collect()
}
