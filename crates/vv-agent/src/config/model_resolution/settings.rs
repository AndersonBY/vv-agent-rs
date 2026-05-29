use serde_json::Value;

use crate::config::{decode_api_key, ConfigError, ResolvedModelConfig};

pub(super) fn normalize_llm_settings(settings: &Value) -> Result<vv_llm::LlmSettings, ConfigError> {
    let normalized = normalized_settings_value(settings)?;
    serde_json::from_value(normalized).map_err(|error| ConfigError::Parse {
        path: "LLM_SETTINGS".to_string(),
        source: Box::new(error),
    })
}

pub(super) fn normalized_settings_value(settings: &Value) -> Result<Value, ConfigError> {
    let settings = settings
        .as_object()
        .and_then(|settings_object| {
            settings_object
                .get("LLM_SETTINGS")
                .filter(|embedded| embedded.get("endpoints").is_some())
                .or(Some(settings))
        })
        .ok_or_else(|| ConfigError::InvalidSettings("settings must be an object".to_string()))?;
    Ok(normalize_settings_value(settings))
}

pub fn build_vv_llm_settings(
    settings: &Value,
    backend: &str,
    resolved: &ResolvedModelConfig,
) -> Result<vv_llm::LlmSettings, ConfigError> {
    let mut normalized = normalized_settings_value(settings)?;
    let object = normalized
        .as_object_mut()
        .ok_or_else(|| ConfigError::InvalidSettings("settings must be an object".to_string()))?;
    let backends = object
        .get_mut("backends")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            ConfigError::InvalidSettings(
                "Invalid LLM settings format: missing backends mapping".to_string(),
            )
        })?;
    let backend_config = backends
        .entry(backend.to_string())
        .or_insert_with(|| serde_json::json!({}));
    let backend_object = backend_config.as_object_mut().ok_or_else(|| {
        ConfigError::InvalidSettings(format!("Backend {backend:?} config is not a mapping"))
    })?;
    let models = backend_object
        .entry("models".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let models_object = models.as_object_mut().ok_or_else(|| {
        ConfigError::InvalidSettings(format!("Backend {backend:?} models is not a mapping"))
    })?;
    let model_config = models_object
        .entry(resolved.selected_model.clone())
        .or_insert_with(|| serde_json::json!({}));
    let model_object = model_config.as_object_mut().ok_or_else(|| {
        ConfigError::InvalidSettings(format!(
            "Model {:?} config is not a mapping",
            resolved.selected_model
        ))
    })?;

    model_object
        .entry("id".to_string())
        .or_insert_with(|| Value::String(resolved.model_id.clone()));
    model_object.insert(
        "endpoints".to_string(),
        Value::Array(
            resolved
                .endpoint_options
                .iter()
                .map(|option| {
                    serde_json::json!({
                        "endpoint_id": option.endpoint.endpoint_id,
                        "model_id": option.model_id,
                    })
                })
                .collect(),
        ),
    );

    if backend_object
        .get("default_endpoint")
        .and_then(Value::as_str)
        .is_none_or(|value| value.is_empty())
    {
        if let Some(endpoint) = resolved.endpoint() {
            backend_object.insert(
                "default_endpoint".to_string(),
                Value::String(endpoint.endpoint_id.clone()),
            );
        }
    }

    serde_json::from_value(normalized).map_err(|error| ConfigError::Parse {
        path: "LLM_SETTINGS".to_string(),
        source: Box::new(error),
    })
}

fn normalize_settings_value(settings: &Value) -> Value {
    let mut normalized = settings.clone();
    let Some(object) = normalized.as_object_mut() else {
        return normalized;
    };

    if !object.contains_key("VERSION") {
        object.insert("VERSION".to_string(), Value::String("2".to_string()));
    }
    if !object.contains_key("backends") {
        if let Some(providers) = object.get("providers").cloned() {
            if providers.is_object() {
                object.insert("backends".to_string(), providers);
            }
        }
    }
    if let Some(endpoints) = object.get_mut("endpoints").and_then(Value::as_array_mut) {
        for endpoint in endpoints {
            let Some(endpoint) = endpoint.as_object_mut() else {
                continue;
            };
            if let Some(api_key) = endpoint.get("api_key").and_then(Value::as_str) {
                endpoint.insert(
                    "api_key".to_string(),
                    Value::String(decode_api_key(api_key)),
                );
            }
        }
    }

    normalized
}
