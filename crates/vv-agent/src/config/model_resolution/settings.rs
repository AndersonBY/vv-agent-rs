use serde_json::Value;

use crate::config::{canonical_settings_value, ConfigError};

pub(super) fn normalize_llm_settings(settings: &Value) -> Result<vv_llm::LlmSettings, ConfigError> {
    let normalized = canonical_settings_value(settings)?;
    serde_json::from_value(normalized).map_err(|error| ConfigError::Parse {
        path: "LLM_SETTINGS".to_string(),
        source: Box::new(error),
    })
}
