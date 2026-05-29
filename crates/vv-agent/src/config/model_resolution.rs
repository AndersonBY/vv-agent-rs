use std::path::Path;

use serde_json::Value;

use crate::llm::{NamedEndpointClientSpec, VvLlmClient};

use super::{
    decode_api_key, load_llm_settings_from_file, ConfigError, EndpointConfig, EndpointOption,
    ResolvedModelConfig,
};

const MODEL_ALIAS_MAP: &[(&str, &str)] = &[("kimi-k2.5", "kimi-k2-thinking")];

pub fn resolve_model_endpoint(
    settings: &Value,
    backend: &str,
    model: &str,
) -> Result<ResolvedModelConfig, ConfigError> {
    let settings = normalize_llm_settings(settings)?;
    let backend_type = backend_type_from_str(backend)?;
    let selected_model = select_model_alias(&settings, backend, model);
    let resolved = settings
        .resolve_chat_model(backend_type, &selected_model)
        .map_err(|error| ConfigError::InvalidSettings(error.to_string()))?;
    Ok(resolved_from_vv_llm(
        backend,
        model,
        &selected_model,
        resolved,
        &settings,
    ))
}

pub fn build_vv_llm_from_local_settings(
    settings_path: impl AsRef<Path>,
    backend: &str,
    model: &str,
    timeout_seconds: f64,
) -> Result<(VvLlmClient, ResolvedModelConfig), ConfigError> {
    let settings_value = load_llm_settings_from_file(settings_path)?;
    let settings = normalize_llm_settings(&settings_value)?;
    let backend_type = backend_type_from_str(backend)?;
    let selected_model = select_model_alias(&settings, backend, model);
    let vv_resolved = settings
        .resolve_chat_model(backend_type, &selected_model)
        .map_err(|error| ConfigError::InvalidSettings(error.to_string()))?;
    let resolved = resolved_from_vv_llm(
        backend,
        model,
        &selected_model,
        vv_resolved.clone(),
        &settings,
    );
    let endpoint_clients = build_endpoint_chat_clients(&vv_resolved, &settings)?;
    let llm = VvLlmClient::new_with_named_endpoint_clients(
        backend,
        resolved.selected_model.clone(),
        resolved.model_id.clone(),
        endpoint_clients,
        timeout_seconds,
    );
    Ok((llm, resolved))
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

fn normalize_llm_settings(settings: &Value) -> Result<vv_llm::LlmSettings, ConfigError> {
    let normalized = normalized_settings_value(settings)?;
    serde_json::from_value(normalized).map_err(|error| ConfigError::Parse {
        path: "LLM_SETTINGS".to_string(),
        source: Box::new(error),
    })
}

fn normalized_settings_value(settings: &Value) -> Result<Value, ConfigError> {
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

fn backend_type_from_str(backend: &str) -> Result<vv_llm::BackendType, ConfigError> {
    match backend {
        "openai" => Ok(vv_llm::BackendType::OpenAI),
        "zhipuai" => Ok(vv_llm::BackendType::ZhiPuAI),
        "minimax" => Ok(vv_llm::BackendType::MiniMax),
        "moonshot" => Ok(vv_llm::BackendType::Moonshot),
        "anthropic" => Ok(vv_llm::BackendType::Anthropic),
        "mistral" => Ok(vv_llm::BackendType::Mistral),
        "deepseek" => Ok(vv_llm::BackendType::DeepSeek),
        "qwen" => Ok(vv_llm::BackendType::Qwen),
        "groq" => Ok(vv_llm::BackendType::Groq),
        "local" => Ok(vv_llm::BackendType::Local),
        "yi" => Ok(vv_llm::BackendType::Yi),
        "gemini" => Ok(vv_llm::BackendType::Gemini),
        "baichuan" => Ok(vv_llm::BackendType::Baichuan),
        "stepfun" => Ok(vv_llm::BackendType::StepFun),
        "xai" => Ok(vv_llm::BackendType::XAI),
        "xiaomi" => Ok(vv_llm::BackendType::Xiaomi),
        "ernie" => Ok(vv_llm::BackendType::Ernie),
        other => Err(ConfigError::UnsupportedBackend(other.to_string())),
    }
}

fn select_model_alias(settings: &vv_llm::LlmSettings, backend: &str, model: &str) -> String {
    if settings
        .backends
        .get(backend)
        .is_some_and(|config| config.models.contains_key(model))
    {
        return model.to_string();
    }
    MODEL_ALIAS_MAP
        .iter()
        .find(|(alias, _)| *alias == model)
        .map(|(_, target)| target.to_string())
        .unwrap_or_else(|| model.to_string())
}

fn resolved_from_vv_llm(
    backend: &str,
    requested_model: &str,
    selected_model: &str,
    resolved: vv_llm::ResolvedModelConfig,
    settings: &vv_llm::LlmSettings,
) -> ResolvedModelConfig {
    let endpoint_options = endpoint_options_from_vv_llm(&resolved, settings);
    ResolvedModelConfig::new(
        backend,
        requested_model,
        selected_model,
        resolved.model_id.clone(),
        endpoint_options,
    )
    .with_token_limits(
        resolved.model.context_length.map(u64::from),
        resolved.model.max_output_tokens.map(u64::from),
    )
}

fn endpoint_options_from_vv_llm(
    resolved: &vv_llm::ResolvedModelConfig,
    settings: &vv_llm::LlmSettings,
) -> Vec<EndpointOption> {
    let mut endpoint_options = resolved
        .model
        .endpoints
        .iter()
        .filter(|binding| binding.enabled())
        .filter_map(|binding| {
            let endpoint = settings
                .endpoints
                .iter()
                .find(|endpoint| endpoint.id == binding.endpoint_id())?;
            Some(EndpointOption {
                endpoint: endpoint_config_from_vv_llm(endpoint.clone()),
                model_id: binding.model_id(&resolved.model.id).to_string(),
            })
        })
        .collect::<Vec<_>>();

    if endpoint_options.is_empty() {
        endpoint_options.push(EndpointOption {
            endpoint: endpoint_config_from_vv_llm(resolved.endpoint.clone()),
            model_id: resolved.model_id.clone(),
        });
    }

    endpoint_options
}

fn build_endpoint_chat_clients(
    resolved: &vv_llm::ResolvedModelConfig,
    settings: &vv_llm::LlmSettings,
) -> Result<Vec<NamedEndpointClientSpec>, ConfigError> {
    endpoint_resolutions_from_vv_llm(resolved, settings)
        .into_iter()
        .map(|endpoint_resolved| {
            let endpoint_id = endpoint_resolved.endpoint.id.clone();
            let model_id = endpoint_resolved.model_id.clone();
            let chat_client = vv_llm::create_chat_client_from_resolved(endpoint_resolved)
                .map_err(|error| ConfigError::InvalidSettings(error.to_string()))?;
            Ok((endpoint_id, model_id, chat_client))
        })
        .collect()
}

fn endpoint_resolutions_from_vv_llm(
    resolved: &vv_llm::ResolvedModelConfig,
    settings: &vv_llm::LlmSettings,
) -> Vec<vv_llm::ResolvedModelConfig> {
    let mut resolutions = resolved
        .model
        .endpoints
        .iter()
        .filter(|binding| binding.enabled())
        .filter_map(|binding| {
            let endpoint = settings
                .endpoints
                .iter()
                .find(|endpoint| endpoint.id == binding.endpoint_id())?;
            Some(vv_llm::ResolvedModelConfig {
                backend: resolved.backend.clone(),
                model: resolved.model.clone(),
                model_id: binding.model_id(&resolved.model.id).to_string(),
                endpoint: endpoint.clone(),
            })
        })
        .collect::<Vec<_>>();

    if resolutions.is_empty() {
        resolutions.push(resolved.clone());
    }

    resolutions
}

fn endpoint_config_from_vv_llm(endpoint: vv_llm::EndpointConfig) -> EndpointConfig {
    EndpointConfig {
        endpoint_id: endpoint.id,
        api_key: endpoint.api_key.unwrap_or_default(),
        api_base: endpoint.api_base.unwrap_or_default(),
        endpoint_type: endpoint
            .endpoint_type
            .unwrap_or_else(|| "default".to_string()),
    }
}
