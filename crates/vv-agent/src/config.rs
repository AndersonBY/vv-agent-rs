use std::fs;
use std::path::Path;

use base64::{engine::general_purpose, Engine as _};
use serde_json::Value;
use thiserror::Error;

use crate::llm::VvLlmClient;

mod python_settings;

const MODEL_ALIAS_MAP: &[(&str, &str)] = &[("kimi-k2.5", "kimi-k2-thinking")];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EndpointConfig {
    pub endpoint_id: String,
    pub api_key: String,
    pub api_base: String,
    pub endpoint_type: String,
}

impl EndpointConfig {
    pub fn new(
        endpoint_id: impl Into<String>,
        api_key: impl Into<String>,
        api_base: impl Into<String>,
    ) -> Self {
        Self {
            endpoint_id: endpoint_id.into(),
            api_key: api_key.into(),
            api_base: api_base.into(),
            endpoint_type: "default".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EndpointOption {
    pub endpoint: EndpointConfig,
    pub model_id: String,
}

impl EndpointOption {
    pub fn new(endpoint: EndpointConfig, model_id: impl Into<String>) -> Self {
        Self {
            endpoint,
            model_id: model_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedModelConfig {
    pub backend: String,
    pub requested_model: String,
    pub selected_model: String,
    pub model_id: String,
    pub endpoint_options: Vec<EndpointOption>,
}

impl ResolvedModelConfig {
    pub fn new(
        backend: impl Into<String>,
        requested_model: impl Into<String>,
        selected_model: impl Into<String>,
        model_id: impl Into<String>,
        endpoint_options: Vec<EndpointOption>,
    ) -> Self {
        Self {
            backend: backend.into(),
            requested_model: requested_model.into(),
            selected_model: selected_model.into(),
            model_id: model_id.into(),
            endpoint_options,
        }
    }

    pub fn endpoint(&self) -> Option<&EndpointConfig> {
        self.endpoint_options.first().map(|option| &option.endpoint)
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("settings file not found: {0}")]
    MissingSettingsFile(String),
    #[error("failed to read settings file {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse settings file {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("invalid LLM settings: {0}")]
    InvalidSettings(String),
    #[error("unsupported chat backend: {0}")]
    UnsupportedBackend(String),
}

pub fn load_llm_settings_from_file(path: impl AsRef<Path>) -> Result<Value, ConfigError> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(ConfigError::MissingSettingsFile(path.display().to_string()));
    }

    let content = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    })?;

    match path.extension().and_then(|ext| ext.to_str()) {
        Some("py") => python_settings::parse_llm_settings_source(&content).map_err(|source| {
            ConfigError::Parse {
                path: path.display().to_string(),
                source: Box::new(source),
            }
        }),
        Some("json") => serde_json::from_str(&content).map_err(|source| ConfigError::Parse {
            path: path.display().to_string(),
            source: Box::new(source),
        }),
        Some("toml") => {
            let value: toml::Value =
                toml::from_str(&content).map_err(|source| ConfigError::Parse {
                    path: path.display().to_string(),
                    source: Box::new(source),
                })?;
            serde_json::to_value(value).map_err(|source| ConfigError::Parse {
                path: path.display().to_string(),
                source: Box::new(source),
            })
        }
        _ => serde_json::from_str(&content).map_err(|source| ConfigError::Parse {
            path: path.display().to_string(),
            source: Box::new(source),
        }),
    }
}

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
    let chat_client = vv_llm::create_chat_client_from_resolved(vv_resolved)
        .map_err(|error| ConfigError::InvalidSettings(error.to_string()))?;
    let llm = VvLlmClient::new(
        backend,
        resolved.selected_model.clone(),
        resolved.model_id.clone(),
        chat_client,
        timeout_seconds,
    );
    Ok((llm, resolved))
}

pub fn build_openai_llm_from_local_settings(
    settings_path: impl AsRef<Path>,
    backend: &str,
    model: &str,
    timeout_seconds: f64,
) -> Result<(VvLlmClient, ResolvedModelConfig), ConfigError> {
    build_vv_llm_from_local_settings(settings_path, backend, model, timeout_seconds)
}

fn normalize_llm_settings(settings: &Value) -> Result<vv_llm::LlmSettings, ConfigError> {
    let settings = settings
        .as_object()
        .and_then(|settings_object| {
            settings_object
                .get("LLM_SETTINGS")
                .filter(|embedded| embedded.get("endpoints").is_some())
                .or(Some(settings))
        })
        .ok_or_else(|| ConfigError::InvalidSettings("settings must be an object".to_string()))?;
    let normalized = normalize_settings_value(settings);
    serde_json::from_value(normalized).map_err(|error| ConfigError::Parse {
        path: "LLM_SETTINGS".to_string(),
        source: Box::new(error),
    })
}

pub fn decode_api_key(raw_value: &str) -> String {
    let raw = raw_value.trim();
    if raw.is_empty() {
        return raw.to_string();
    }

    if let Some(direct) = extract_suffix_key(raw) {
        return direct;
    }

    if std::env::var("V_AGENT_ENABLE_BASE64_KEY_DECODE").as_deref() == Ok("1") {
        if let Some(decoded) = maybe_base64_decode(raw) {
            if let Some(from_decoded) = extract_suffix_key(&decoded) {
                return from_decoded;
            }
            if looks_like_api_key(&decoded) {
                return decoded;
            }
        }
    }

    raw.to_string()
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

fn extract_suffix_key(value: &str) -> Option<String> {
    let (_, suffix) = value.split_once(':')?;
    let suffix = suffix.trim();
    looks_like_api_key(suffix).then(|| suffix.to_string())
}

fn maybe_base64_decode(value: &str) -> Option<String> {
    let mut padded = value.to_string();
    let remainder = padded.len() % 4;
    if remainder != 0 {
        padded.extend(std::iter::repeat_n('=', 4 - remainder));
    }
    let decoded = general_purpose::STANDARD.decode(padded).ok()?;
    String::from_utf8(decoded).ok()
}

fn looks_like_api_key(value: &str) -> bool {
    !value.is_empty() && value.len() >= 10 && !value.chars().any(char::is_whitespace)
}
